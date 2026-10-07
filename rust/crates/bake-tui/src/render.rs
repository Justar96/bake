//! Draws [`App`] into a Ratatui frame. Rows from the top: body, gap, notice,
//! header, the composer box (top edge, draft rows, bottom edge), the
//! sample-agents row, and status. Short terminals drop chrome before the
//! first draft row; the body takes what remains.

use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::activity::{self, Tones};
use crate::app::{App, Focus, Notice, SAMPLE_AGENTS, agent};
use crate::composer::{self, ComposerWindow, Edge};
use crate::editor::{self, display};
use crate::frame::FrameStyle;

const MIN_BODY_ROWS: u16 = 3;
/// Cells between the terminal's edges and rows drawn outside the box, so
/// they align with the box's contents.
const INSET: u16 = 2;

mod copy {
    pub const TITLE: &str = "Bake · Rust preview";
    pub const INTRO: &[&str] = &[
        "Try the native composer and sample-agent views. Models and tools are not connected yet.",
        "",
        "Press Tab to inspect a sample agent. Esc brings you back to your draft.",
    ];
    pub const HEADER_COMPOSER: &str = "Rust preview · model not connected";
    pub const HEADER_LIST: &str = "Sample agents";
    pub const HEADER_LIST_HINT: &str = "↑↓ select · Enter inspect · Esc back";
    pub const HEADER_INSPECT_HINT: &str = "Esc back to draft";
    pub const TAB_HINT: &str = "Tab sample agents";
    /// The empty draft's placeholder; parts after the first drop whole when
    /// they do not fit.
    pub const PLACEHOLDER: &[&str] = &["Type a draft", "Alt+Enter newline", "Ctrl+Z undo"];
    pub const DRAFT_KEPT: &str = "draft kept · Esc returns";
    pub const STOP_SAMPLE: &str = "Esc stops the sample";
    /// Phases a sample activity steps through, one every [`PHASE_SECONDS`].
    pub const SAMPLE_PHASES: &[&str] =
        &["thinking", "writing", "running bash", "running subagent +2"];
    pub const PHASE_SECONDS: u64 = 4;
    pub const ABOVE: &str = "above";
    pub const BELOW: &str = "below";
    pub const NO_MODEL: &str =
        "Model connection is not available in this preview. Your draft is kept.";
    pub const READ_ONLY: &str =
        "Read-only inspection: input does not reach any agent or your draft. Esc returns.";
    pub const LIST_KEYS: &str = "↑↓ select · Enter inspect · Esc returns to the draft.";
    pub const DRAFT_LIMIT: &str = "Draft limit reached; the rest of the input was not added.";
    pub const LIST_TITLE: &str = "Sample agents · fixed examples, nothing is running";
    pub const INSPECT_PARENT: &str = "Parent: this preview's draft, unchanged while you inspect.";
    pub const INSPECT_READ_ONLY: &str =
        "Read-only. Typing here never reaches an agent or the parent draft.";
    pub const INSPECT_RETURN: &str = "Esc returns to the draft · Tab returns to the list.";
    pub const STATE: &str = "static sample";
    pub const STATUS: &[&str] = &[
        "rust preview",
        "model not connected",
        "Ctrl+C quit",
        concat!("v", env!("CARGO_PKG_VERSION")),
    ];
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}

/// Presentation state that depends on the terminal, the frame's geometry, or
/// time, and persists between frames.
#[derive(Debug, Default)]
pub struct View {
    pub frame: FrameStyle,
    /// Colours the activity line may use; [`Tones::None`] also stops its shimmer.
    pub tones: Tones,
    /// The current time, on the clock the caller passes with events.
    pub now: Duration,
    window: ComposerWindow,
}

impl View {
    pub fn new(frame: FrameStyle, tones: Tones) -> Self {
        Self {
            frame,
            tones,
            now: Duration::ZERO,
            window: ComposerWindow::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Rows {
    body: u16,
    gap: u16,
    notice: u16,
    header: u16,
    rule: u16,
    composer: u16,
    base_rule: u16,
    agents: u16,
    status: u16,
}

/// Grants the first draft row, then chrome by priority, then further draft
/// rows up to [`composer::window_rows`] while the body keeps
/// [`MIN_BODY_ROWS`], then the body. The box's two edges are claimed
/// together so a short terminal loses the open edge first.
fn plan(height: u16, draft_rows: u16, notice: bool) -> Rows {
    fn take(left: &mut u16, want: u16) -> u16 {
        let got = want.min(*left);
        *left -= got;
        got
    }
    let mut remaining = height;
    let left = &mut remaining;
    let mut rows = Rows {
        composer: take(left, 1),
        header: take(left, 1),
        notice: take(left, u16::from(notice)),
        status: take(left, 1),
        rule: take(left, 1),
        base_rule: take(left, 1),
        agents: take(left, 1),
        gap: take(left, 1),
        ..Rows::default()
    };
    let extra = draft_rows.clamp(1, composer::window_rows(height)) - 1;
    rows.composer += take(left, extra.min(left.saturating_sub(MIN_BODY_ROWS)));
    rows.body = take(left, u16::MAX);
    rows
}

pub fn render(frame: &mut Frame, app: &App, view: &mut View) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let geometry = composer::geometry(area.width);
    let draft = editor::layout(app.draft.text(), app.draft.caret(), geometry.wrap_width);
    let rows = plan(
        area.height,
        u16::try_from(draft.rows.len()).unwrap_or(u16::MAX),
        app.notice.is_some(),
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
    let header = next(rows.header);
    let rule = next(rows.rule);
    let band = next(rows.composer);
    let base_rule = next(rows.base_rule);
    let agents = next(rows.agents);
    let status = next(rows.status);
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
    render_header(app, view, inset(header), buf);
    render_agents_row(inset(agents), buf);
    render_status(inset(status), buf);

    let visible = usize::from(band.height);
    let total = draft.rows.len();
    let top = view
        .window
        .follow(draft.caret.0, visible, total, geometry.wrap_width);
    let above = top;
    let below = total.saturating_sub(top + visible);

    // The box closes only when both edges have rows; otherwise the edges that
    // fit are plain rules. Hidden-row counts ride the edges, so they never
    // take a column from the draft.
    let glyphs = view.frame.glyphs();
    let closed = geometry.boxed && rule.height > 0 && base_rule.height > 0;
    let above_label = (above > 0).then(|| format!("+{above} {}", copy::ABOVE));
    let below_label = if below > 0 {
        Some(format!("+{below} {}", copy::BELOW))
    } else {
        match app.focus {
            Focus::Inspect(_) => Some(copy::DRAFT_KEPT.to_owned()),
            Focus::Composer if app.activity.is_some() => Some(copy::STOP_SAMPLE.to_owned()),
            _ => None,
        }
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
    let text_style = if editing { Style::new() } else { dim() };
    let prompt_style = if editing { accent() } else { dim() };
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
            Line::styled(placeholder(usize::from(text_width)), dim()),
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

/// The placeholder's parts that fit `width`, whole; the first part is always
/// drawn and clipped if it must be.
fn placeholder(width: usize) -> String {
    let mut text = copy::PLACEHOLDER[0].to_owned();
    for part in &copy::PLACEHOLDER[1..] {
        if text.width() + " · ".width() + part.width() > width {
            break;
        }
        text.push_str(" · ");
        text.push_str(part);
    }
    text
}

fn line(buf: &mut Buffer, area: Rect, content: Line) {
    if !area.is_empty() {
        buf.set_line(area.x, area.y, &content, area.width);
    }
}

/// Left text, then right text when both fit with two spaces between them.
fn split_row(buf: &mut Buffer, area: Rect, left: Line, right: Line) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let (lw, rw) = (left.width(), right.width());
    line(buf, area, left);
    if lw + 2 + rw <= width {
        let x = area.x + (width - rw) as u16;
        line(buf, Rect::new(x, area.y, rw as u16, 1), right);
    }
}

/// The activity line: the word, shimmering, then its phase and elapsed time,
/// dim. The right-hand hint gives way first, then the phase and time, so
/// the word is never cut for them.
fn activity_row(
    buf: &mut Buffer,
    area: Rect,
    view: &View,
    word: &str,
    details: &str,
    elapsed: Duration,
) {
    let mut spans = activity::shimmer_spans(word, elapsed, view.tones, view.tones.moves());
    if word.width() + 2 + details.width() <= usize::from(area.width) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(details.to_owned(), dim()));
    }
    split_row(
        buf,
        area,
        Line::from(spans),
        Line::styled(copy::TAB_HINT, dim()),
    );
}

fn render_header(app: &App, view: &View, area: Rect, buf: &mut Buffer) {
    match app.focus {
        Focus::Composer => match app.activity {
            Some(sample) => {
                let elapsed = view.now.saturating_sub(sample.started);
                let step = elapsed.as_secs() / copy::PHASE_SECONDS;
                let phase = copy::SAMPLE_PHASES[step as usize % copy::SAMPLE_PHASES.len()];
                let details = format!("{phase} · {}", activity::format_elapsed(elapsed));
                activity_row(
                    buf,
                    area,
                    view,
                    &format!("{}…", sample.word),
                    &details,
                    elapsed,
                );
            }
            None => split_row(
                buf,
                area,
                Line::from(copy::HEADER_COMPOSER.bold()),
                Line::styled(copy::TAB_HINT, dim()),
            ),
        },
        Focus::AgentList => split_row(
            buf,
            area,
            Line::from(copy::HEADER_LIST.bold()),
            Line::styled(copy::HEADER_LIST_HINT, dim()),
        ),
        Focus::Inspect(id) => {
            let name = agent(id).map_or(id, |a| a.name);
            split_row(
                buf,
                area,
                Line::from(vec![
                    "Inspecting ".bold(),
                    Span::styled(name, accent().bold()),
                    Span::styled(" · read-only", dim()),
                ]),
                Line::styled(copy::HEADER_INSPECT_HINT, dim()),
            );
        }
    }
}

fn render_agents_row(area: Rect, buf: &mut Buffer) {
    split_row(
        buf,
        area,
        Line::styled(
            format!("↳ Sample agents {} · {}", SAMPLE_AGENTS.len(), copy::STATE),
            dim(),
        ),
        Line::styled("Tab", dim()),
    );
}

/// Whole fields, two spaces apart, while they fit; never a cut field.
fn render_status(area: Rect, buf: &mut Buffer) {
    let mut text = String::new();
    for field in copy::STATUS {
        let sep = if text.is_empty() { 0 } else { 2 };
        if text.width() + sep + field.width() > usize::from(area.width) {
            break;
        }
        if sep > 0 {
            text.push_str("  ");
        }
        text.push_str(field);
    }
    line(buf, area, Line::styled(text, dim()));
}

fn render_body(app: &App, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let mut lines: Vec<Line> = Vec::new();
    match app.focus {
        Focus::Composer => {
            lines.push(Line::styled(copy::TITLE, accent().bold()));
            lines.push(Line::default());
            lines.extend(copy::INTRO.iter().map(|t| Line::raw(*t)));
        }
        Focus::AgentList => {
            lines.push(Line::styled(copy::LIST_TITLE, dim()));
            for a in SAMPLE_AGENTS {
                let selected = a.id == app.selected;
                let style = if selected {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::new()
                };
                lines.push(Line::from(vec![
                    Span::raw(if selected { "› " } else { "  " }),
                    Span::styled(a.name, style),
                    Span::styled(format!("  {}  {}", a.id, copy::STATE), dim()),
                ]));
            }
            lines.push(Line::default());
            let selected = app.selected_agent();
            lines.extend(selected.detail.iter().map(|t| Line::styled(*t, dim())));
        }
        Focus::Inspect(id) => {
            let a = agent(id).unwrap_or(&SAMPLE_AGENTS[0]);
            lines.push(Line::from(vec![
                Span::styled(a.name, accent().bold()),
                Span::styled(format!("  {}  {}", a.id, copy::STATE), dim()),
            ]));
            lines.push(Line::styled(
                copy::INSPECT_READ_ONLY,
                Style::new().fg(Color::Yellow),
            ));
            lines.push(Line::styled(copy::INSPECT_PARENT, dim()));
            lines.push(Line::default());
            lines.extend(a.detail.iter().map(|t| Line::raw(*t)));
            lines.push(Line::default());
            lines.push(Line::styled(copy::INSPECT_RETURN, dim()));
        }
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Columns where boxed draft text starts.
    const TEXT_X: u16 = 4;

    fn draw(app: &App, width: u16, height: u16) -> (Vec<String>, Position) {
        draw_with(app, &mut View::default(), width, height)
    }

    fn draw_with(app: &App, view: &mut View, width: u16, height: u16) -> (Vec<String>, Position) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app, view)).unwrap();
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

    fn key(app: &mut App, code: KeyCode) {
        app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn row_index(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not found in {rows:#?}"))
    }

    fn lines(count: usize) -> App {
        let mut app = App::default();
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
            let (rows, cursor) = draw(&App::default(), w, h);
            assert!(rows[0].contains("Rust preview"));
            assert!(rows.iter().any(|r| r.contains("Sample agents")));
            let prompt = row_index(&rows, "│ ❯ Type a draft");
            assert_eq!(cursor, Position::new(TEXT_X, prompt as u16));
            assert!(row_index(&rows, "model not connected") < prompt);
            assert!(rows[usize::from(h) - 1].contains("rust preview"));
        }
    }

    #[test]
    fn the_composer_is_a_closed_box_aligned_with_the_rows_around_it() {
        let mut app = App::default();
        app.draft.type_text("hello");
        let (rows, _) = draw(&app, 40, 12);
        let prompt = row_index(&rows, "❯ hello");
        assert_eq!(rows[prompt - 1], format!("╭{}╮", "─".repeat(38)));
        assert_eq!(rows[prompt], format!("│ ❯ hello{}│", " ".repeat(30)));
        assert_eq!(rows[prompt + 1], format!("╰{}╯", "─".repeat(38)));
        // Header, agents row, and status start where the prompt does.
        assert!(rows[prompt - 2].starts_with("  Rust preview"));
        assert!(rows[prompt + 2].starts_with("  ↳ Sample agents"));
        assert!(rows[prompt + 3].starts_with("  rust preview"));
        // Right-hand keys end where the box's contents do.
        let (rows, _) = draw(&app, 80, 24);
        let prompt = row_index(&rows, "❯ hello");
        assert!(rows[prompt - 2].ends_with("Tab sample agents  "));
        assert!(rows[prompt + 2].ends_with("Tab  "));
    }

    #[test]
    fn classic_frames_draw_ascii_edges_and_prompt() {
        let mut app = App::default();
        app.draft.type_text("hi");
        let mut view = View::new(FrameStyle::Classic, Tones::default());
        let (rows, _) = draw_with(&app, &mut view, 20, 12);
        let prompt = row_index(&rows, "| > hi");
        assert_eq!(rows[prompt - 1], format!("+{}+", "-".repeat(18)));
        assert!(rows[prompt].ends_with(" |"));
        assert_eq!(rows[prompt + 1], format!("+{}+", "-".repeat(18)));
    }

    #[test]
    fn refused_submission_shows_the_notice_above_the_header() {
        let mut app = App::default();
        app.draft.type_text("keep me");
        key(&mut app, KeyCode::Enter);
        let (rows, cursor) = draw(&app, 80, 24);
        let notice = row_index(&rows, "Model connection is not available");
        let header = row_index(&rows, "Rust preview · model not connected");
        let prompt = row_index(&rows, "❯ keep me");
        assert!(notice < header && header < prompt);
        assert_eq!(cursor, Position::new(TEXT_X + 7, prompt as u16));
    }

    #[test]
    fn long_narrow_draft_keeps_the_caret_on_its_last_visible_row() {
        let mut app = App::default();
        app.draft.paste(&"界e\u{301}".repeat(60));
        app.draft.newline();
        app.draft.type_text("end");
        let (rows, cursor) = draw(&app, 40, 12);
        let base_rule = rows.len() - 3;
        assert_eq!(usize::from(cursor.y), base_rule - 1);
        assert!(rows[base_rule - 1].contains("end"));
        assert_eq!(cursor.x, TEXT_X + 3);
        assert!(rows[0].contains("Rust preview"));
        // The top edge counts the rows above the window.
        assert!(rows[row_index(&rows, "above")].starts_with('╭'));
    }

    #[test]
    fn the_window_holds_still_while_the_caret_moves_inside_it() {
        let mut app = lines(10);
        let mut view = View::default();
        let (rows, _) = draw_with(&app, &mut view, 80, 24);
        assert!(rows.iter().any(|r| r.contains("line5")));
        assert!(!rows.iter().any(|r| r.contains("line4")));
        assert!(rows[row_index(&rows, "line5")].contains("│ ^ line5"));
        assert!(rows.iter().any(|r| r.contains("+5 above")));
        // Up to line8, then line5: the text stays put and the caret moves.
        for _ in 0..2 {
            key(&mut app, KeyCode::Home);
            key(&mut app, KeyCode::Left);
        }
        let (rows, cursor) = draw_with(&app, &mut view, 80, 24);
        assert!(!rows.iter().any(|r| r.contains("line4")));
        assert_eq!(usize::from(cursor.y), row_index(&rows, "line7"));
        for _ in 0..3 {
            key(&mut app, KeyCode::Home);
            key(&mut app, KeyCode::Left);
        }
        let (rows, cursor) = draw_with(&app, &mut view, 80, 24);
        assert_eq!(usize::from(cursor.y), row_index(&rows, "line4"));
        assert!(rows.iter().any(|r| r.contains("+1 below")));
        assert!(rows[row_index(&rows, "line8")].contains("v line8"));
    }

    #[test]
    fn the_caret_never_changes_the_composer_height() {
        // Exactly one full row of text at 40 columns: 34 cells.
        let mut app = App::default();
        app.draft.type_text(&"x".repeat(34));
        let (at_end, end_cursor) = draw(&app, 40, 12);
        key(&mut app, KeyCode::Home);
        let (at_start, start_cursor) = draw(&app, 40, 12);
        assert_eq!(at_end, at_start);
        assert_eq!(end_cursor.y, start_cursor.y);
        // The caret after a full row sits on the padding cell, inside the box.
        assert_eq!(end_cursor.x, 38);
    }

    #[test]
    fn resizing_rewraps_the_draft_and_keeps_the_caret_in_the_box() {
        let mut app = App::default();
        app.draft.paste(&"word ".repeat(80));
        let mut view = View::default();
        for (w, h) in [(80, 24), (30, 10), (12, 8), (11, 8), (120, 40), (40, 12)] {
            let (rows, cursor) = draw_with(&app, &mut view, w, h);
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
        let app = lines(20);
        for (height, shown) in [(24, 5), (40, 8), (60, 12)] {
            let (rows, _) = draw(&app, 80, height);
            let count = rows.iter().filter(|r| r.contains("line")).count();
            assert_eq!(count, shown, "{height} rows");
        }
    }

    #[test]
    fn inspector_says_read_only_and_hides_the_caret_from_the_draft() {
        let mut app = App::default();
        app.draft.type_text("draft");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Enter);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render(f, &app, &mut View::default()))
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
        let (rows, _) = draw(&app, 80, 24);
        assert!(
            rows.iter()
                .any(|r| r.contains("Inspecting Sample explorer · read-only"))
        );
        assert!(rows.iter().any(|r| r.contains("❯ draft")));
        assert!(
            rows.iter()
                .any(|r| r.starts_with('╰') && r.contains("draft kept · Esc returns"))
        );
    }

    fn sampling(word: &'static str) -> App {
        let mut app = App::default();
        app.activity = Some(crate::app::SampleActivity {
            word,
            started: Duration::from_secs(1),
        });
        app
    }

    /// The header's cell styles at `now`.
    fn header_styles(app: &App, tones: Tones, now: Duration) -> Vec<Style> {
        let mut view = View::new(FrameStyle::Round, tones);
        view.now = now;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, app, &mut view)).unwrap();
        let (rows, _) = draw_with(app, &mut view, 80, 24);
        let y = row_index(&rows, "Kneading…") as u16;
        let buffer = terminal.backend().buffer();
        (2..11).map(|x| buffer[(x, y)].style()).collect()
    }

    #[test]
    fn sample_activity_is_text_with_its_phase_and_time() {
        let app = sampling("Kneading");
        let mut view = View {
            now: Duration::from_millis(6_500),
            ..View::default()
        };
        let (rows, _) = draw_with(&app, &mut view, 80, 24);
        let header = row_index(&rows, "Kneading…");
        assert!(rows[header].starts_with("  Kneading…  writing · 5s"));
        assert!(rows[header].ends_with("Tab sample agents  "));
        assert!(
            rows.iter()
                .any(|r| r.starts_with('╰') && r.contains(" Esc stops the sample "))
        );
        // No glyph stands for the activity: the words carry it.
        let braille = |c: char| ('\u{2800}'..='\u{28FF}').contains(&c);
        assert!(!rows.iter().any(|r| r.chars().any(braille)));
        // Narrow, the phase and time give way before the word.
        let (rows, _) = draw_with(&app, &mut view, 20, 12);
        assert_eq!(
            rows[row_index(&rows, "Kneading…")].trim_end(),
            "  Kneading…"
        );
    }

    #[test]
    fn the_shimmer_moves_across_the_word_unless_color_is_off() {
        let app = sampling("Kneading");
        let early = header_styles(
            &app,
            Tones::TrueColor,
            Duration::from_secs(1) + activity::BEAT * 3,
        );
        let later = header_styles(
            &app,
            Tones::TrueColor,
            Duration::from_secs(1) + activity::BEAT * 7,
        );
        assert_ne!(early, later);
        let brightest = |styles: &[Style]| {
            styles
                .iter()
                .position(|s| s.fg == Tones::TrueColor.style(1.0).fg)
        };
        assert!(brightest(&early) < brightest(&later));
        let still = header_styles(
            &app,
            Tones::None,
            Duration::from_secs(1) + activity::BEAT * 3,
        );
        let still_later = header_styles(
            &app,
            Tones::None,
            Duration::from_secs(1) + activity::BEAT * 7,
        );
        assert_eq!(still, still_later);
    }

    #[test]
    fn agent_list_marks_the_selected_row() {
        let mut app = App::default();
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Down);
        let (rows, _) = draw(&app, 40, 12);
        assert!(rows.iter().any(|r| r.starts_with("› Sample reviewer")));
        assert!(rows.iter().any(|r| r.starts_with("  Sample explorer")));
    }

    #[test]
    fn the_placeholder_drops_parts_whole() {
        assert_eq!(
            placeholder(80),
            "Type a draft · Alt+Enter newline · Ctrl+Z undo"
        );
        assert_eq!(placeholder(40), "Type a draft · Alt+Enter newline");
        assert_eq!(placeholder(20), "Type a draft");
        assert_eq!(placeholder(4), "Type a draft");
    }

    #[test]
    fn small_terminals_keep_the_input_row_without_panicking() {
        let mut app = App::default();
        app.draft
            .paste("emoji \u{1F468}\u{200D}\u{1F469} 你好 กำลัง\nsecond line");
        key(&mut app, KeyCode::Enter);
        for width in 0..=12 {
            for height in 0..=8 {
                let (rows, cursor) = draw(&app, width, height);
                if width > 0 && height > 0 {
                    assert!(cursor.x < width && cursor.y < height, "{width}x{height}");
                    assert!(rows.iter().all(|r| r.width() <= usize::from(width)));
                }
            }
        }
        let (rows, cursor) = draw(&app, 40, 1);
        assert_eq!(cursor.y, 0);
        assert!(rows[0].contains("second"));
    }

    #[test]
    fn plan_never_exceeds_the_height() {
        for height in 0..80 {
            for draft in 1..20 {
                let r = plan(height, draft, true);
                let sum = r.body
                    + r.gap
                    + r.header
                    + r.notice
                    + r.rule
                    + r.composer
                    + r.base_rule
                    + r.agents
                    + r.status;
                assert_eq!(sum, height);
                assert!(r.composer <= composer::window_rows(height));
            }
        }
    }
}
