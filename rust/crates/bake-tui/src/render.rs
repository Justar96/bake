//! Draws [`App`] into a Ratatui frame. Rows from the top: body, gap, header,
//! notice, rule, composer, rule, sample-agents row, status. Short terminals
//! drop chrome before the first composer row; the body takes what remains.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, Notice, SAMPLE_AGENTS, agent};
use crate::editor::{self, display};

/// Visible composer rows before the draft scrolls under the caret.
pub const MAX_COMPOSER_ROWS: u16 = 5;
const MIN_BODY_ROWS: u16 = 3;
const PROMPT: &str = "❯ ";
const PROMPT_WIDTH: u16 = 2;

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
    pub const PLACEHOLDER: &str = "Type a draft · Alt+Enter newline · Ctrl+Z undo";
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

#[derive(Clone, Copy, Debug, Default)]
struct Rows {
    body: u16,
    gap: u16,
    header: u16,
    notice: u16,
    rule: u16,
    composer: u16,
    base_rule: u16,
    agents: u16,
    status: u16,
}

/// Grants the first composer row, then chrome by priority, then extra draft
/// rows while the body keeps [`MIN_BODY_ROWS`], then the body.
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
        agents: take(left, 1),
        base_rule: take(left, 1),
        gap: take(left, 1),
        ..Rows::default()
    };
    let extra = draft_rows.clamp(1, MAX_COMPOSER_ROWS) - 1;
    rows.composer += take(left, extra.min(left.saturating_sub(MIN_BODY_ROWS)));
    rows.body = take(left, u16::MAX);
    rows
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let prompt_width = if area.width > PROMPT_WIDTH + 1 {
        PROMPT_WIDTH
    } else {
        0
    };
    let draft_width = usize::from(area.width - prompt_width);
    let draft = editor::layout(app.draft.text(), app.draft.caret(), draft_width);
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
    let header = next(rows.header);
    let notice = next(rows.notice);
    let rule = next(rows.rule);
    let composer = next(rows.composer);
    let base_rule = next(rows.base_rule);
    let agents = next(rows.agents);
    let status = next(rows.status);

    let buf = frame.buffer_mut();
    render_body(app, body, buf);
    render_header(app, header, buf);
    if let Some(kind) = app.notice {
        let text = match kind {
            Notice::NoModel => copy::NO_MODEL,
            Notice::ReadOnly => copy::READ_ONLY,
            Notice::ListKeys => copy::LIST_KEYS,
            Notice::DraftLimit => copy::DRAFT_LIMIT,
        };
        line(
            buf,
            notice,
            Line::styled(text, Style::new().fg(Color::Yellow)),
        );
    }
    for r in [rule, base_rule] {
        let bar = "─".repeat(usize::from(r.width));
        line(buf, r, Line::styled(bar, dim()));
    }
    render_agents_row(agents, buf);
    render_status(status, buf);

    // Draft rows: keep the caret's row in the window, pinned to its bottom
    // once the draft is taller than the window.
    let visible = usize::from(composer.height);
    let top = (draft.caret.0 + 1).saturating_sub(visible);
    let editing = app.focus == Focus::Composer;
    let text_style = if editing { Style::new() } else { dim() };
    for (i, row) in draft.rows.iter().skip(top).take(visible).enumerate() {
        let y = composer.y + i as u16;
        if prompt_width > 0 {
            let marker = if i == 0 && top == 0 { PROMPT } else { "  " };
            buf.set_string(composer.x, y, marker, accent());
        }
        let cell = Rect::new(composer.x + prompt_width, y, area.width - prompt_width, 1);
        let text = display(&app.draft.text()[row.start..row.end]);
        line(buf, cell, Line::styled(text, text_style));
    }
    if composer.height > 0 && app.draft.is_empty() {
        let cell = Rect::new(
            composer.x + prompt_width,
            composer.y,
            area.width - prompt_width,
            1,
        );
        line(buf, cell, Line::styled(copy::PLACEHOLDER, dim()));
    }
    if editing && composer.height > 0 {
        let (row, col) = draft.caret;
        frame.set_cursor_position(Position::new(
            composer.x + prompt_width + col as u16,
            composer.y + (row - top) as u16,
        ));
    }
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

fn render_header(app: &App, area: Rect, buf: &mut Buffer) {
    match app.focus {
        Focus::Composer => split_row(
            buf,
            area,
            Line::from(copy::HEADER_COMPOSER.bold()),
            Line::styled(copy::TAB_HINT, dim()),
        ),
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

    fn draw(app: &App, width: u16, height: u16) -> (Vec<String>, Position) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
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

    #[test]
    fn first_screen_names_the_preview_and_sample_agents() {
        for (w, h) in [(80, 24), (120, 36)] {
            let (rows, cursor) = draw(&App::default(), w, h);
            assert!(rows[0].contains("Rust preview"));
            assert!(rows.iter().any(|r| r.contains("Sample agents")));
            let prompt = row_index(&rows, "❯");
            assert_eq!(cursor, Position::new(PROMPT_WIDTH, prompt as u16));
            assert!(row_index(&rows, "model not connected") < prompt);
            assert!(rows[usize::from(h) - 1].contains("rust preview"));
        }
    }

    #[test]
    fn refused_submission_shows_the_notice_above_the_kept_draft() {
        let mut app = App::default();
        app.draft.type_text("keep me");
        key(&mut app, KeyCode::Enter);
        let (rows, cursor) = draw(&app, 80, 24);
        let notice = row_index(&rows, "Model connection is not available");
        let prompt = row_index(&rows, "❯ keep me");
        assert!(notice < prompt);
        assert_eq!(cursor, Position::new(PROMPT_WIDTH + 7, prompt as u16));
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
        assert_eq!(cursor.x, PROMPT_WIDTH + 3);
        assert!(rows[0].contains("Rust preview"));
    }

    #[test]
    fn inspector_says_read_only_and_hides_the_caret_from_the_draft() {
        let mut app = App::default();
        app.draft.type_text("draft");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Enter);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        let (rows, _) = draw(&app, 80, 24);
        assert!(
            rows.iter()
                .any(|r| r.contains("Inspecting Sample explorer · read-only"))
        );
        assert!(rows.iter().any(|r| r.contains("❯ draft")));
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
        for height in 0..40 {
            for draft in 1..10 {
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
                assert!(r.composer <= MAX_COMPOSER_ROWS);
            }
        }
    }
}
