//! Draws [`State`] into a Ratatui frame. Rows from the top: body, gap, notice,
//! the approval panel while a request waits, the bar (activity and status),
//! the composer box (top edge, draft rows, bottom edge), and the
//! sample-agents row, in the heights [`crate::layout::plan`] grants them.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Position, Rect};
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::terminal::Frame;
use ratatui_core::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::activity::{self, Hue, Tones};
use crate::completion;
use crate::composer::{self, Edge};
use crate::copy;
use crate::dialog::{Dialog, DialogKind, InfoLine, ListLine, SearchOrder};
use crate::editor::{self, display};
use crate::frame::FrameStyle;
use crate::jobs;
use crate::layout::{self, Needs};
use crate::mention::PathKind;
use crate::mode::{self, HINT_MIN_COLUMNS};
use crate::runtime::Target;
use crate::selection::line_columns;
use crate::state::{
    ActivityKind, Area, DraftSpot, FileMenu, Focus, Notice, Outcome, SAMPLE_AGENTS, SampleAgent,
    ScrollTrack, Spot, State, agent,
};
use crate::status::{self, Tone};
use crate::transcript::{self, Look, TagTone};

/// Cells between the terminal's edges and rows drawn outside the box, so
/// they align with the box's contents.
const INSET: u16 = 2;
/// Transcript rows retained in the inline live region before the controls.
const INLINE_BODY_ROWS: u16 = 8;

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}

/// Draws one frame, records its interaction geometry, and follows the draft
/// caret within the composer rows this frame grants it.
pub fn render(app: &mut State, frame: &mut Frame) {
    render_region(app, frame, None, frame.area().height);
}

/// Draws the existing preview components in an inline held region. Settled
/// rows already in terminal scrollback are excluded while following output.
pub fn render_inline(app: &mut State, frame: &mut Frame, published: usize, terminal_height: u16) {
    render_region(app, frame, Some(published), terminal_height);
}

fn needs(app: &State, width: u16, height: u16) -> Needs {
    let geometry = composer::geometry(width);
    let draft = editor::layout(app.draft.text(), app.draft.caret(), geometry.wrap_width);
    Needs {
        draft_rows: u16::try_from(draft.rows.len()).unwrap_or(u16::MAX),
        // The sample agents' row belongs to the preview; a live session's
        // shows its background shells while any runs, or one is open.
        standing: !app.live || app.jobs_row() || matches!(app.focus, Focus::Jobs | Focus::Shell),
        notice: u16::from(app.notice.is_some() || app.quitting()),
        pending: panel_rows(app.pending.len()),
        panel: panel_rows(app.attachments.len()),
        menu: menu_rows(app),
        approval: approval_rows(app, inner_width(width)).max(dialog_rows(
            app,
            inner_width(width),
            height,
        )),
    }
}

/// Widest a sheet grows, the TypeScript `SHEET_WIDTH`: past this, rows
/// are harder to scan than to scroll.
pub const SHEET_WIDTH: u16 = 80;

/// The fewest columns a framed sheet takes; a narrower one is drawn
/// without its frame, as the TypeScript sheet is below its minimum.
const SHEET_FRAME_COLUMNS: u16 = 12;

/// Columns between a sheet's columns.
const SHEET_GAP: usize = 2;

/// Body rows a sheet asks for at most at `height`: half the terminal, at
/// least 3 and at most 14, as the TypeScript picker takes up to half the
/// screen for a list worth scanning.
fn sheet_body_cap(height: u16) -> usize {
    usize::from(height / 2).clamp(3, 14)
}

/// The most rows a list's note or a field's prompt wraps to; past them,
/// the last row ends in `…`.
const SHEET_TEXT_ROWS: usize = 6;

/// A list's note or a field's prompt as a sheet `width` columns wide shows
/// it: each line wrapped to the framed sheet's inner width, so a notice the
/// person must read, such as what an upload sends, is shown whole rather
/// than cut at the edge, and at most [`SHEET_TEXT_ROWS`] rows.
fn sheet_text(text: &str, width: u16) -> Vec<String> {
    let room = usize::from(width.min(SHEET_WIDTH)).saturating_sub(4).max(1);
    let mut rows: Vec<String> = text
        .split('\n')
        .flat_map(|line| transcript::wrap(&editor::sanitize(line).replace(['\r', '\t'], " "), room))
        .collect();
    if rows.len() > SHEET_TEXT_ROWS {
        rows.truncate(SHEET_TEXT_ROWS);
        if let Some(last) = rows.last_mut() {
            *last = fit(&format!("{last}…"), room);
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// The rows a dialog draws around its body at `width` columns, frame
/// excluded: a list's title, note, filter, selected row's detail, and
/// keys; a field's title, prompt, field, error, and keys; an information
/// sheet's title and keys.
fn sheet_chrome(dialog: &Dialog, width: u16) -> usize {
    match &dialog.request.kind {
        DialogKind::List(list) => {
            2 + list
                .note
                .as_deref()
                .map_or(0, |note| sheet_text(note, width).len())
                + usize::from(list.search.is_some())
                + usize::from(list.filterable)
                + usize::from(list.items.iter().any(|item| item.about.is_some()))
        }
        DialogKind::Field(field) => {
            3 + sheet_text(&field.prompt, width).len() + usize::from(field.error.is_some())
        }
        DialogKind::Info(_) => 2,
    }
}

/// The body lines a dialog has: a list's unfiltered lines, so typing a
/// filter never changes the sheet's height, or an information sheet's
/// rows wrapped to a framed sheet's width at `width` columns.
fn sheet_body(dialog: &Dialog, width: u16) -> usize {
    match &dialog.request.kind {
        DialogKind::List(_) => dialog.all_lines().len().max(1),
        DialogKind::Field(_) => 0,
        DialogKind::Info(info) => {
            let inner = usize::from(width.min(SHEET_WIDTH)).saturating_sub(4);
            info_rows(info, inner).len().max(1)
        }
    }
}

/// The rows the open dialog wants at `width` columns and `height` rows
/// while no approval waits, its frame included: its chrome and as many
/// body lines as fit [`sheet_body_cap`].
fn dialog_rows(app: &State, width: u16, height: u16) -> u16 {
    let Some(dialog) = app.dialog.as_ref().filter(|_| app.approval().is_none()) else {
        return 0;
    };
    let rows =
        sheet_chrome(dialog, width) + sheet_body(dialog, width).min(sheet_body_cap(height)) + 2;
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// The fewest cells a sheet's rows need to show a selection, a field, or
/// a value legibly; a narrower sheet is not handed the keyboard.
const SHEET_MIN_INNER: usize = 10;

/// An information sheet's rows at `width` cells, as the TypeScript sheet
/// wraps its lines: each value wrapped under its own column, its wrapped
/// rows hanging past the label, so a long path or id is shown whole.
fn info_rows(info: &crate::dialog::InfoDialog, width: usize) -> Vec<Line<'static>> {
    let clean = |text: &str| editor::sanitize(text).replace(['\n', '\t'], " ");
    let width = width.max(1);
    let label_w = info
        .lines
        .iter()
        .filter_map(|l| match l {
            InfoLine::Field { label, .. } => Some(clean(label).width()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .min(width / 3);
    let gap = SHEET_GAP.min(width.saturating_sub(label_w));
    let room = width.saturating_sub(label_w + gap).max(1);
    let mut rows = Vec::new();
    for info_line in &info.lines {
        match info_line {
            InfoLine::Heading(text) => rows.push(Line::styled(
                fit(&clean(text), width),
                Style::new().add_modifier(Modifier::BOLD),
            )),
            InfoLine::Field { label, value } => {
                let value = clean(value);
                let wrapped = transcript::wrap(&value, room);
                for (index, part) in wrapped.into_iter().enumerate() {
                    let label = if index == 0 {
                        clean(label)
                    } else {
                        String::new()
                    };
                    rows.push(Line::from(vec![
                        Span::styled(pad(&label, label_w), dim()),
                        Span::raw(" ".repeat(gap)),
                        Span::raw(part),
                    ]));
                }
            }
            InfoLine::Blank => rows.push(Line::raw("")),
        }
    }
    rows
}

/// `text` cut to `width` cells from its middle, keeping its start and its
/// end, so two routes or ids that share a long prefix stay told apart.
fn fit_middle(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width < 5 {
        return fit(text, width);
    }
    let head = (width - 1) / 3;
    let tail = width - 1 - head;
    let start = fit(text, head + 1);
    let start = start.trim_end_matches('…');
    let end = fit_start(text, tail + 1);
    format!("{start}…{}", end.trim_start_matches('…'))
}

/// `text` cut to `width` cells, then padded to them.
fn pad(text: &str, width: usize) -> String {
    let cut = fit(text, width);
    let fill = width.saturating_sub(cut.width());
    format!("{cut}{}", " ".repeat(fill))
}

/// `text` right-aligned in `width` cells.
fn pad_start(text: &str, width: usize) -> String {
    let cut = fit(text, width);
    format!("{}{cut}", " ".repeat(width.saturating_sub(cut.width())))
}

/// The first body line shown and how many: every line when they fit;
/// otherwise a window that keeps `follow` in view, moving only when it
/// would leave, with a row above and below to say what is hidden when
/// there are three rows or more.
fn sheet_window(
    total: usize,
    rows: usize,
    follow: Option<usize>,
    top: usize,
) -> (usize, usize, bool) {
    if total <= rows {
        return (0, total, false);
    }
    let edges = rows >= 3;
    let count = if edges { rows - 2 } else { rows };
    if count == 0 {
        return (0, 0, false);
    }
    let mut top = top.min(total - count);
    if let Some(follow) = follow {
        if follow < top {
            top = follow;
        } else if follow >= top + count {
            top = follow + 1 - count;
        }
    }
    (top, count, edges)
}

/// A dialog's key row: `keys`, or `short` where they do not fit, or only
/// Enter and Esc ([`copy::DIALOG_KEYS_TINY`]), with the selection's
/// position at the right edge; where even that does not fit, the position
/// goes before Esc does, so a narrow sheet still says how to cancel.
fn sheet_footer(keys: &str, short: &str, position: Option<String>, width: usize) -> Line<'static> {
    let candidates = [keys, short, copy::DIALOG_KEYS_TINY];
    if let Some(position) = position {
        for keys in candidates {
            if keys.width() + SHEET_GAP + position.width() <= width {
                let fill = width - keys.width() - position.width();
                return Line::styled(format!("{keys}{}{position}", " ".repeat(fill)), dim());
            }
        }
    }
    let keys = candidates
        .into_iter()
        .find(|keys| keys.width() <= width)
        .unwrap_or(copy::DIALOG_KEYS_TINY);
    Line::styled(fit(keys, width), dim())
}

/// The status column's style: the TypeScript picker's `done` tone.
fn current_style(tones: crate::activity::Tones) -> Style {
    match tones {
        crate::activity::Tones::TrueColor => Style::new().fg(Color::Rgb(0x22, 0xc5, 0x5e)),
        crate::activity::Tones::Ansi => Style::new().fg(Color::Green),
        crate::activity::Tones::None => Style::new(),
    }
}

/// How a list's rows lay out their columns, sized over every item so the
/// columns stay put while the list scrolls or narrows.
struct SheetColumns {
    label: usize,
    facts: Vec<usize>,
    status: usize,
    /// Whether anything follows the label, so it has a fixed column.
    fixed: bool,
    /// Whether rows show their detail.
    detail: bool,
}

/// The TypeScript picker's columns at `width` cells after the pointer:
/// the label takes at most 55% of the room the facts and status leave,
/// and facts give way from the right before a label is cut shorter than
/// 20 cells (or its own width).
fn sheet_columns(
    list: &crate::dialog::ListDialog,
    width: usize,
    clean: &dyn Fn(&str) -> String,
) -> SheetColumns {
    let status = if list.current.is_some() {
        copy::DIALOG_CURRENT.width()
    } else {
        0
    };
    let widest = list
        .items
        .iter()
        .map(|item| clean(&item.label).width())
        .max()
        .unwrap_or(1)
        .max(1);
    let columns = list
        .items
        .iter()
        .map(|item| item.facts.len())
        .max()
        .unwrap_or(0);
    let mut facts: Vec<usize> = (0..columns)
        .map(|column| {
            list.items
                .iter()
                .map(|item| item.facts.get(column).map_or(0, |fact| clean(fact).width()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let room = |facts: &[usize]| {
        width
            .saturating_sub(if status > 0 { status + SHEET_GAP } else { 0 })
            .saturating_sub(
                facts
                    .iter()
                    .map(|w| if *w > 0 { w + SHEET_GAP } else { 0 })
                    .sum::<usize>(),
            )
    };
    while !facts.is_empty() && room(&facts) * 55 / 100 < widest.min(20) {
        facts.pop();
    }
    let room = room(&facts);
    // The detail column gives way next, before the label is cut shorter
    // than 24 cells (or its own width); the selected row's description,
    // under the list, still says what it is.
    let detail =
        list.items.iter().any(|item| item.detail.is_some()) && room * 55 / 100 >= widest.min(24);
    let fixed = detail || status > 0 || facts.iter().any(|w| *w > 0);
    let label = if !fixed {
        room.max(1)
    } else if detail {
        (room * 55 / 100).min(widest).max(1)
    } else {
        widest
            .min(
                room.saturating_sub(if status > 0 || facts.iter().any(|w| *w > 0) {
                    SHEET_GAP
                } else {
                    0
                }),
            )
            .max(1)
    };
    SheetColumns {
        label,
        facts,
        status,
        fixed,
        detail,
    }
}

/// The open dialog ([`crate::dialog`]), in the approval panel's place,
/// drawn as the TypeScript sheet and picker draw theirs
/// (`apps/tui/packages/ui/src/sheet.tsx`, `picker.tsx`): at most
/// [`SHEET_WIDTH`] columns, in a dim frame where it fits; its title bold;
/// a list's note, filter row (`>` and what is typed, or what typing does),
/// rows under their groups' headings with the selection's `▸`, aligned
/// label, fact, and `Current` columns, and a dim detail, the selected
/// row's description, and keys with the selection's position. A long list
/// scrolls only when the selection would leave it, and its edges say how
/// many rows they hide. A secret field is drawn as one `•` per character
/// typed, at every width.
///
/// Records the dialog as on screen only when its title, every row around
/// its body, its keys, and (for a list or an information sheet) at least
/// one body line fit, so the keyboard is handed to it only once the
/// selection and its keys are shown.
fn render_dialog(app: &mut State, area: Rect, buf: &mut Buffer) {
    app.dialog_on_screen = None;
    if area.is_empty() || app.approval().is_some() {
        return;
    }
    let tones = app.tones;
    let glyphs = app.frame.glyphs();
    let ready = app.dialog_ready();
    let Some(dialog) = app.dialog.as_mut() else {
        return;
    };
    let clean = |text: &str| editor::sanitize(text).replace(['\n', '\t'], " ");
    let width = area.width.min(SHEET_WIDTH);
    let chrome = sheet_chrome(dialog, width);
    let least = chrome + usize::from(!matches!(dialog.request.kind, DialogKind::Field(_)));
    let framed = width >= SHEET_FRAME_COLUMNS && usize::from(area.height) >= least + 2;
    let inner = if framed {
        let frame = dim();
        let horizontal = glyphs.horizontal.repeat(usize::from(width - 2));
        line(
            buf,
            Rect::new(area.x, area.y, width, 1),
            Line::styled(
                format!("{}{horizontal}{}", glyphs.top_left, glyphs.top_right),
                frame,
            ),
        );
        line(
            buf,
            Rect::new(area.x, area.bottom() - 1, width, 1),
            Line::styled(
                format!("{}{horizontal}{}", glyphs.bottom_left, glyphs.bottom_right),
                frame,
            ),
        );
        for y in area.y + 1..area.bottom() - 1 {
            line(
                buf,
                Rect::new(area.x, y, 1, 1),
                Line::styled(glyphs.vertical, frame),
            );
            line(
                buf,
                Rect::new(area.x + width - 1, y, 1, 1),
                Line::styled(glyphs.vertical, frame),
            );
        }
        Rect::new(area.x + 2, area.y + 1, width - 4, area.height - 2)
    } else {
        Rect::new(area.x, area.y, width, area.height)
    };
    let w = usize::from(inner.width);
    let body_rows = usize::from(inner.height).saturating_sub(chrome);
    let whole = usize::from(inner.height) >= least && usize::from(inner.width) >= SHEET_MIN_INNER;
    let title_style = tone_style(Tone::Waiting, tones).add_modifier(Modifier::BOLD);
    let mut head: Vec<Line> = Vec::new();
    let mut body: Vec<Line> = Vec::new();
    let mut tail: Vec<Line> = Vec::new();
    let (keys, short, position) = match dialog.request.kind.clone() {
        DialogKind::List(list) => {
            head.push(Line::styled(fit(&clean(&list.title), w), title_style));
            if let Some(note) = &list.note {
                for row in sheet_text(note, width) {
                    head.push(Line::styled(fit(&row, w), dim()));
                }
            }
            // A session search's state, or why its query matches nothing.
            let search = dialog.search.clone().filter(|_| list.search.is_some());
            if let Some(view) = &search {
                head.push(match &view.error {
                    Some(error) => Line::styled(
                        fit(&clean(&error.message()), w),
                        Style::new().fg(Color::Yellow),
                    ),
                    None => Line::styled(
                        fit(
                            &copy::SESSION_SEARCH_STATE
                                .replace(
                                    "{order}",
                                    match view.order {
                                        SearchOrder::Recent => copy::SESSION_SEARCH_RECENT,
                                        SearchOrder::Relevance => copy::SESSION_SEARCH_RELEVANCE,
                                    },
                                )
                                .replace("{named}", if view.named_only { "on" } else { "off" }),
                            w,
                        ),
                        dim(),
                    ),
                });
            }
            if list.filterable {
                let prompt = Span::styled("> ", accent().add_modifier(Modifier::BOLD));
                let placeholder = if search.is_some() {
                    copy::SESSION_SEARCH_FILTER
                } else {
                    copy::DIALOG_FILTER
                };
                head.push(if dialog.filter().is_empty() {
                    Line::from(vec![
                        prompt,
                        Span::styled(fit(placeholder, w.saturating_sub(2)), dim()),
                    ])
                } else {
                    Line::from(vec![
                        prompt,
                        Span::raw(fit_start(&clean(dialog.filter()), w.saturating_sub(2))),
                    ])
                });
            }
            let rows = dialog.rows();
            if dialog.selected >= rows.len() {
                dialog.selected = rows.len().saturating_sub(1);
            }
            let lines = dialog.lines();
            let follow = lines
                .iter()
                .position(|l| matches!(l, ListLine::Row { row, .. } if *row == dialog.selected));
            let (mut top, count, edges) = sheet_window(lines.len(), body_rows, follow, dialog.top);
            // The first row of a run brings its heading into view.
            if let Some(follow) = follow
                && follow == top
                && top > 0
                && count >= 2
                && matches!(lines[top - 1], ListLine::Heading { .. })
            {
                top -= 1;
            }
            dialog.top = top;
            dialog.page = count.max(1);
            let pointer_w = 2;
            let columns = sheet_columns(&list, w.saturating_sub(pointer_w), &clean);
            let hidden = |range: &[ListLine]| {
                range
                    .iter()
                    .filter(|l| matches!(l, ListLine::Row { .. }))
                    .count()
            };
            let heading = |group: usize, count: usize| -> Line<'static> {
                let name = clean(list.items[group].group.as_deref().unwrap_or(""));
                let name = fit(&name, w);
                let rest = format!(" {count} ");
                let rule = w.saturating_sub(name.width() + rest.width());
                Line::from(vec![
                    Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(
                        fit(
                            &format!("{rest}{}", "─".repeat(rule)),
                            w.saturating_sub(w.min(rest.width())),
                        ),
                        dim(),
                    ),
                ])
            };
            if rows.is_empty() {
                let empty = match &search {
                    Some(view) if view.named_only && dialog.filter().trim().is_empty() => {
                        copy::SESSION_SEARCH_NO_NAMED
                    }
                    Some(_) => copy::SESSION_SEARCH_NO_MATCH,
                    None => copy::DIALOG_NO_MATCH,
                };
                body.push(Line::styled(fit(&format!("  {empty}"), w), dim()));
            }
            if edges {
                let above = hidden(&lines[..top]);
                let more = if above > 0 {
                    format!("↑ {above} {}", copy::DIALOG_MORE)
                } else {
                    String::new()
                };
                // Scrolled into a run, the edge names the run it cut.
                let carried = match lines.get(top) {
                    Some(ListLine::Row { index, .. }) if above > 0 => {
                        list.items[*index].group.clone()
                    }
                    _ => None,
                };
                body.push(match carried {
                    Some(group) => Line::from(vec![
                        Span::styled(
                            fit(&clean(&group), w),
                            Style::new().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("  {more}"), dim()),
                    ]),
                    None => Line::styled(fit(&format!("  {more}"), w), dim()),
                });
            }
            for list_line in lines.iter().skip(top).take(count) {
                match *list_line {
                    ListLine::Heading { group, count } => body.push(heading(group, count)),
                    ListLine::Row { row, index } => {
                        let item = &list.items[index];
                        let active = row == dialog.selected;
                        let mut spans = vec![Span::styled(
                            if active { "▸ " } else { "  " },
                            accent().add_modifier(Modifier::BOLD),
                        )];
                        let label_style = if active {
                            accent().add_modifier(Modifier::BOLD)
                        } else {
                            Style::new()
                        };
                        let label = clean(&item.label);
                        if columns.fixed {
                            spans.push(Span::styled(pad(&label, columns.label), label_style));
                            spans.push(Span::raw(" ".repeat(SHEET_GAP)));
                        } else {
                            spans.push(Span::styled(fit(&label, columns.label), label_style));
                        }
                        let fact_style = if active { Style::new() } else { dim() };
                        for (column, fact_w) in columns.facts.iter().enumerate() {
                            if *fact_w == 0 {
                                continue;
                            }
                            let fact = item.facts.get(column).map(|f| clean(f)).unwrap_or_default();
                            spans.push(Span::styled(pad_start(&fact, *fact_w), fact_style));
                            spans.push(Span::raw(" ".repeat(SHEET_GAP)));
                        }
                        if columns.status > 0 {
                            let status = if list.current == Some(index) {
                                copy::DIALOG_CURRENT
                            } else {
                                ""
                            };
                            spans.push(Span::styled(
                                pad(status, columns.status),
                                current_style(tones),
                            ));
                            spans.push(Span::raw(" ".repeat(SHEET_GAP)));
                        }
                        if let Some(detail) = item.detail.as_ref().filter(|_| columns.detail) {
                            let used: usize = spans.iter().map(|span| span.content.width()).sum();
                            let room = w.saturating_sub(used);
                            if room > 1 {
                                spans.push(Span::styled(
                                    fit_middle(&clean(detail), room),
                                    fact_style,
                                ));
                            }
                        }
                        body.push(Line::from(spans));
                    }
                }
            }
            if edges {
                let below = hidden(lines.get(top + count..).unwrap_or(&[]));
                body.push(Line::styled(
                    fit(
                        &if below > 0 {
                            format!("  ↓ {below} {}", copy::DIALOG_MORE)
                        } else {
                            String::new()
                        },
                        w,
                    ),
                    dim(),
                ));
            }
            if list.items.iter().any(|item| item.about.is_some()) {
                let about = rows
                    .get(dialog.selected)
                    .and_then(|&index| list.items[index].about.as_deref())
                    .map(clean)
                    .unwrap_or_default();
                tail.push(Line::styled(
                    format!("  {}", fit_middle(&about, w.saturating_sub(2))),
                    dim(),
                ));
            }
            let position =
                (!rows.is_empty()).then(|| format!("{}/{}", dialog.selected + 1, rows.len()));
            if list.saves_default {
                (
                    copy::DIALOG_LIST_SAVE_KEYS,
                    copy::DIALOG_LIST_SAVE_KEYS_SHORT,
                    position,
                )
            } else {
                (
                    copy::DIALOG_LIST_KEYS,
                    copy::DIALOG_LIST_KEYS_SHORT,
                    position,
                )
            }
        }
        DialogKind::Field(field) => {
            head.push(Line::styled(fit(&clean(&field.title), w), title_style));
            for row in sheet_text(&field.prompt, width) {
                head.push(Line::raw(fit(&row, w)));
            }
            let shown = if field.secret {
                "•".repeat(dialog.text.chars())
            } else {
                clean(dialog.text.expose())
            };
            // The end of a long entry stays in view, where typing goes.
            let room = w.saturating_sub(2).max(1);
            let tail_text: String = {
                let chars: Vec<char> = shown.chars().collect();
                let skip = chars.len().saturating_sub(room);
                chars[skip..].iter().collect()
            };
            head.push(Line::from(vec![
                Span::styled("> ", accent().add_modifier(Modifier::BOLD)),
                Span::raw(fit_start(&tail_text, room)),
            ]));
            if let Some(error) = &field.error {
                head.push(Line::styled(
                    fit(&clean(error), w),
                    Style::new().fg(Color::Yellow),
                ));
            }
            (copy::DIALOG_FIELD_KEYS, copy::DIALOG_FIELD_KEYS_SHORT, None)
        }
        DialogKind::Info(info) => {
            head.push(Line::styled(fit(&clean(&info.title), w), title_style));
            let rendered = info_rows(&info, w);
            let total = rendered.len();
            let page = body_rows.max(1);
            dialog.top = dialog.top.min(total.saturating_sub(page));
            dialog.page = page;
            dialog.total = total;
            body.extend(rendered.into_iter().skip(dialog.top).take(body_rows));
            if total > body_rows {
                let shown = format!(
                    "{}-{}/{total}",
                    dialog.top + 1,
                    (dialog.top + body_rows).min(total)
                );
                (
                    copy::DIALOG_INFO_SCROLL_KEYS,
                    copy::DIALOG_INFO_KEYS,
                    Some(shown),
                )
            } else {
                (copy::DIALOG_INFO_KEYS, copy::DIALOG_INFO_KEYS, None)
            }
        }
    };
    let footer = if dialog.answered {
        sheet_footer(copy::DIALOG_WAITING, copy::DIALOG_WAITING, None, w)
    } else if !ready {
        sheet_footer(copy::DIALOG_PREPARING, copy::DIALOG_PREPARING, None, w)
    } else {
        sheet_footer(keys, short, position, w)
    };
    let id = dialog.request.id;
    // Top to bottom; short of rows, the body gives way first, and the
    // keys keep the last row.
    let room = usize::from(inner.height).saturating_sub(1);
    let rows_drawn = head
        .into_iter()
        .chain(body.into_iter().take(body_rows))
        .chain(tail)
        .take(room);
    for (y, row) in (inner.y..).zip(rows_drawn) {
        line(buf, Rect::new(inner.x, y, inner.width, 1), row);
    }
    if inner.height > 0 {
        line(
            buf,
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            footer,
        );
    }
    if whole {
        app.dialog_on_screen = Some(id);
    }
}

/// The columns rows outside the box get at `width`, inside its insets.
fn inner_width(width: u16) -> u16 {
    if composer::geometry(width).boxed {
        width.saturating_sub(2 * INSET)
    } else {
        width
    }
}

/// Detail rows the approval panel asks for; a longer detail scrolls.
const APPROVAL_DETAIL_ROWS: usize = 12;

/// The rows the approval panel wants: its title, its detail up to
/// [`APPROVAL_DETAIL_ROWS`], and its keys.
fn approval_rows(app: &State, width: u16) -> u16 {
    if app.approval().is_none() {
        return 0;
    }
    let detail = approval_detail(app, width)
        .len()
        .clamp(1, APPROVAL_DETAIL_ROWS);
    u16::try_from(detail + 2).unwrap_or(u16::MAX)
}

/// The displayed request's detail as rows of at most `width` cells: the
/// reason, then the call's arguments under a heading, each in full. The
/// text is sanitized, so model-written text cannot reach the terminal as
/// an escape sequence; tabs become spaces.
/// Characters of a call id an approval's head shows for the `codemode`
/// call that made the call it asks about; the call's own id beside it
/// already holds the whole id.
const SHORT_ID_CHARS: usize = 12;

/// `id` kept to [`SHORT_ID_CHARS`], the last of them an ellipsis when cut.
fn short_id(id: &str) -> String {
    if id.chars().count() <= SHORT_ID_CHARS {
        return id.to_owned();
    }
    let kept: String = id.chars().take(SHORT_ID_CHARS - 1).collect();
    format!("{kept}…")
}

fn approval_detail(app: &State, width: u16) -> Vec<String> {
    let Some(approval) = app.approval() else {
        return Vec::new();
    };
    let width = usize::from(width.max(1));
    let wrap = |text: &str| -> Vec<String> {
        editor::sanitize(text)
            .replace('\t', "  ")
            .split('\n')
            .flat_map(|part| transcript::wrap(part, width))
            .collect()
    };
    let mut rows = wrap(&approval.reason);
    if !approval.arguments.trim().is_empty() {
        rows.push(copy::APPROVAL_ARGUMENTS.to_owned());
        rows.extend(wrap(&approval.arguments));
    }
    rows
}

/// The approval panel for the displayed request, as the TypeScript
/// `ApprovalView` orders it: what is asked (the tool and its exact call
/// id, with the queue's length when others wait and the codemode call it
/// came from when that fits), its detail, scrolled to
/// [`State::approval_scroll`] with a row that says where while it does not
/// all fit, then the keys. Records which request it showed whole: its
/// title, at least one detail row, and its keys.
fn render_approval(app: &mut State, area: Rect, buf: &mut Buffer) {
    app.approval_on_screen = None;
    let Some(approval) = app.approval().cloned() else {
        return;
    };
    if area.is_empty() {
        return;
    }
    let clean = |text: &str| editor::sanitize(text).replace(['\n', '\t'], " ");
    let waiting = tone_style(Tone::Waiting, app.tones).add_modifier(Modifier::BOLD);
    let mut head = vec![
        Span::styled(
            format!("{}: {}", copy::APPROVAL_TITLE, clean(&approval.tool)),
            waiting,
        ),
        Span::styled(format!("  {}", clean(&approval.call_id)), dim()),
    ];
    if app.approvals.len() > 1 {
        head.push(Span::styled(format!("  1/{}", app.approvals.len()), dim()));
    }
    // The codemode call it came from comes last, and only whole: the
    // call's own id already holds it, so it gives way to the queue count.
    if let Some(parent) = &approval.parent_call_id {
        let from = Span::styled(
            format!(
                "  {} {}",
                copy::APPROVAL_FROM_CODEMODE,
                short_id(&clean(parent))
            ),
            dim(),
        );
        let used: usize = head.iter().map(Span::width).sum();
        if used + from.width() <= usize::from(area.width) {
            head.push(from);
        }
    }
    line(
        buf,
        Rect::new(area.x, area.y, area.width, 1),
        Line::from(head),
    );
    if area.height < 3 {
        return;
    }
    let detail = approval_detail(app, area.width);
    let rows = usize::from(area.height - 2);
    // A detail longer than its rows gives its last row to where the view
    // is and the keys that move it, cut to fit, so even a narrow terminal
    // shows that more is there.
    let more = detail.len() > rows && rows >= 2;
    let page = rows - usize::from(more);
    let last = detail.len().saturating_sub(page);
    app.approval_scroll = app.approval_scroll.min(last);
    app.approval_page = page;
    let first = app.approval_scroll;
    for (i, row) in detail.iter().skip(first).take(page).enumerate() {
        let style = if row == copy::APPROVAL_ARGUMENTS {
            dim()
        } else {
            Style::new()
        };
        line(
            buf,
            Rect::new(area.x, area.y + 1 + i as u16, area.width, 1),
            Line::styled(row.clone(), style),
        );
    }
    if more {
        let position = format!(
            "{}-{}/{} · {}",
            first + 1,
            (first + page).min(detail.len()),
            detail.len(),
            copy::APPROVAL_SCROLL
        );
        line(
            buf,
            Rect::new(area.x, area.y + 1 + page as u16, area.width, 1),
            Line::styled(fit(&position, usize::from(area.width)), dim()),
        );
    }
    let keys = if app.approval_chosen() {
        Line::from(vec![
            Span::styled(copy::APPROVAL_CONFIRM, waiting),
            Span::styled(format!(" · {}", copy::APPROVAL_CONFIRM_KEYS), dim()),
        ])
    } else if app.approval_armed() {
        Line::styled(copy::APPROVAL_KEYS, dim())
    } else {
        // Not armed yet: keys cannot answer, so none are offered but Esc.
        Line::styled(copy::APPROVAL_PREPARING, dim())
    };
    line(
        buf,
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
        keys,
    );
    app.approval_on_screen = Some(approval.generation);
}

/// Natural controls plus a bounded transcript, with the same composer window
/// policy as the fullscreen preview. Short terminals retain its collapse order.
pub fn inline_height(app: &State, width: u16, terminal_height: u16) -> u16 {
    let rows = layout::plan(terminal_height, needs(app, width, terminal_height));
    // Keep navigation's measured viewport stable when live output settles;
    // the held controls still grow with the draft and its panels.
    let body = rows.body.min(INLINE_BODY_ROWS);
    rows.total() - rows.body + body
}

fn render_region(
    app: &mut State,
    frame: &mut Frame,
    published: Option<usize>,
    terminal_height: u16,
) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let geometry = composer::geometry(area.width);
    let draft = editor::layout(app.draft.text(), app.draft.caret(), geometry.wrap_width);
    let mut rows = layout::plan(terminal_height, needs(app, area.width, terminal_height));
    if published.is_some() {
        rows.body = area.height.saturating_sub(rows.total() - rows.body);
    }
    app.transcript_published = published.unwrap_or(0);

    let mut y = area.y;
    let mut next = |height: u16| {
        let rect = Rect::new(area.x, y, area.width, height);
        y += height;
        rect
    };
    let body = next(rows.body);
    let gap = next(rows.gap);
    let pending = next(rows.pending);
    let panel = next(rows.panel);
    let notice = next(rows.notice);
    let menu_area = next(rows.menu);
    let approval_area = next(rows.approval);
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
    app.transcript_area = None;
    render_body(app, body, gap, buf, published);
    // The prompt to press Ctrl+C again takes the notice's row while it is
    // armed; the notice returns when it lapses.
    if app.quitting() {
        line(
            buf,
            inset(notice),
            Line::styled(copy::QUIT, tone_style(Tone::Waiting, app.tones)),
        );
    } else if let Some(kind) = app.notice {
        let text: &str = match kind {
            Notice::Refused => app.refusal.as_deref().unwrap_or(copy::RETURNED),
            Notice::SampleOnly => copy::SAMPLE_ONLY,
            Notice::NoImages => copy::NO_IMAGES,
            Notice::ReadOnly => copy::READ_ONLY,
            Notice::DraftLimit => copy::DRAFT_LIMIT,
            Notice::NoClipboardImage => copy::NO_CLIPBOARD_IMAGE,
            Notice::NoCommands => copy::NO_COMMANDS,
            Notice::PendingSent => copy::PENDING_SENT,
            Notice::NoPending => copy::NO_PENDING,
            Notice::ImagesWhileRunning => copy::IMAGES_WHILE_RUNNING,
            Notice::PreviewOnly => copy::PREVIEW_ONLY,
            Notice::Returned => copy::RETURNED,
            Notice::Restored => copy::RESTORED,
            Notice::ApprovalKeys => copy::APPROVAL_KEYS_ONLY,
            Notice::ApprovalChooseFirst => copy::APPROVAL_CHOOSE_FIRST,
        };
        line(
            buf,
            inset(notice),
            Line::styled(text, Style::new().fg(Color::Yellow)),
        );
    }
    render_pending(app, inset(pending), buf);
    render_attachments(app, inset(panel), buf);
    render_menu(app, inset(menu_area), buf);
    render_approval(app, inset(approval_area), buf);
    render_dialog(app, inset(approval_area), buf);
    let columns = area.width;
    render_bar(app, inset(bar), buf);
    if app.live {
        render_jobs_row(app, columns, inset(agents), buf);
    } else {
        render_agents_row(columns, inset(agents), buf);
    }

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
    let listing = matches!(app.focus, Focus::AgentList | Focus::Jobs | Focus::Shell);
    let approving = app.approval().is_some();
    let asking = approving || app.dialog.is_some();
    let mut mode = mode::view(app.mode(), !app.draft.is_empty(), area.width);
    if approving {
        mode.placeholder = copy::PLACEHOLDER_APPROVAL;
        mode.hint = (area.width >= mode::HINT_MIN_COLUMNS).then_some(copy::HINT_APPROVAL);
    } else if asking {
        mode.placeholder = copy::PLACEHOLDER_DIALOG;
        mode.hint = (area.width >= mode::HINT_MIN_COLUMNS).then_some(copy::HINT_DIALOG);
    } else if app.handing_off() {
        // A dialog closed: the composer takes keys once this frame is
        // armed, and says so until then.
        mode.placeholder = copy::PLACEHOLDER_PREPARING;
        mode.hint = (area.width >= mode::HINT_MIN_COLUMNS).then_some(copy::HINT_PREPARING);
    }
    let below_label = if below > 0 {
        Some(format!("+{below} {}", copy::BELOW))
    } else {
        // While the menu offers commands, its key takes the hint's place.
        let completing = !asking
            && !app.handing_off()
            && (app.menu().is_some_and(|menu| !menu.entries.is_empty())
                || app.file_menu().is_some_and(|menu| !menu.entries.is_empty()));
        mode.hint
            .filter(|_| !listing)
            .map(|hint| {
                if completing {
                    copy::TAB_COMPLETES
                } else {
                    hint
                }
            })
            .map(str::to_owned)
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

    // A waiting approval takes the keyboard: the draft stays, dimmed and
    // without a caret, until the request is settled.
    let editing = app.focus == Focus::Composer && !asking && !app.handing_off();
    let quiet = mode.dim || listing || asking || app.handing_off();
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
                    ActivityKind::Turn => (Hue::Running, copy::SAMPLE_PHASES),
                    ActivityKind::Compaction => (Hue::Compacting, copy::COMPACTING_PHASES),
                };
                // A runtime turn shows the phase its runtime reported; a
                // sample steps through its own.
                let step = elapsed.as_secs() / copy::PHASE_SECONDS;
                let phase = match (&app.phase, sample.runtime) {
                    (Some(phase), true) => phase.as_str(),
                    (None, true) => copy::FIRST_PHASE,
                    (_, false) => phases[step as usize % phases.len()],
                };
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
                    Outcome::Failed => (format!("✗ {}", copy::FAILED_TURN), Color::Red),
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
        Focus::Jobs => (
            vec![Span::styled(
                copy::JOBS_TITLE,
                Style::new().add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(format!("  {}", copy::JOBS_LIST_KEYS), dim())],
        ),
        Focus::Shell => (
            vec![
                Span::styled(
                    copy::JOBS_TITLE,
                    accent().add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled(" / ", dim()),
                Span::styled(
                    app.selected_job()
                        .map_or_else(String::new, |job| job.id.clone()),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ],
            vec![Span::styled(format!("  {}", copy::JOBS_SHELL_KEYS), dim())],
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

/// Rows a menu of `count` entries lists: up to [`completion::MENU_ROWS`]
/// and a count of the rest.
fn listed_rows(count: usize) -> usize {
    count.min(completion::MENU_ROWS) + usize::from(count > completion::MENU_ROWS)
}

/// The file menu's status rows, as the oracle stacks them under its list:
/// nothing found, still looking, and why discovery failed.
fn file_status(menu: &FileMenu) -> Vec<(String, bool)> {
    let mut rows = Vec::new();
    if menu.entries.is_empty() && !menu.loading {
        rows.push((copy::NO_FILES.to_owned(), false));
    }
    if menu.loading {
        rows.push((copy::FILES_LOADING.to_owned(), false));
    }
    if let Some(error) = &menu.error {
        rows.push((format!("{}: {error}", copy::FILES_ERROR), true));
    }
    rows
}

/// Rows the menus want: the slash menu's commands, or one row saying none
/// match; the file menu's mentions and its status rows; or, once the slash
/// menu has closed on a typed command, its usage line.
fn menu_rows(app: &State) -> u16 {
    let rows = if let Some(menu) = app.menu() {
        listed_rows(menu.entries.len()).max(1)
    } else if let Some(menu) = app.file_menu() {
        (listed_rows(menu.entries.len()) + file_status(&menu).len()).max(1)
    } else if app.focus == Focus::Composer {
        usize::from(completion::usage(app.draft.text(), app.catalog()).is_some())
    } else {
        0
    };
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// One status row under a menu, past the rail: dim, or in the failed red.
fn status_row(app: &State, text: &str, error: bool, area: Rect, buf: &mut Buffer) {
    let style = if error {
        match app.tones {
            Tones::TrueColor => Style::new().fg(Color::Rgb(0xef, 0x44, 0x44)),
            Tones::Ansi => Style::new().fg(Color::Red),
            Tones::None => Style::new(),
        }
    } else {
        dim()
    };
    line(
        buf,
        area,
        Line::styled(fit(&format!("  {text}"), usize::from(area.width)), style),
    );
}

/// `text` cut from its start to `width` cells, with `…` standing for what
/// was cut, as Ink's `truncate-start` cuts a long path to keep its name.
fn fit_start(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for grapheme in text.graphemes(true).rev() {
        let cells = grapheme.width();
        if used + cells + 1 > width {
            break;
        }
        kept.push(grapheme);
        used += cells;
    }
    if width == 0 {
        return String::new();
    }
    kept.reverse();
    format!("…{}", kept.concat())
}

/// The file menu, drawn as the slash menu is: a `▸` on the selected row, the
/// mentions in a column sized to the longest, up to 40 cells, a long one cut
/// from its start; each row's kind after it only when the rows shown mix
/// files and directories. Its status rows follow.
fn render_file_menu(app: &State, menu: &FileMenu, area: Rect, buf: &mut Buffer) {
    let asking = tone_style(Tone::Asking, app.tones);
    let status = file_status(menu);
    let rows = usize::from(area.height).saturating_sub(status.len());
    let count = menu.entries.len();
    let mut y = area.y;
    if count > 0 && rows > 0 {
        let shown = if count > rows {
            rows.saturating_sub(1).max(1)
        } else {
            count
        };
        let first = (menu.selected + 1).saturating_sub(shown).min(count - shown);
        let visible = &menu.entries[first..first + shown];
        let mixed = visible.iter().any(|c| c.kind != visible[0].kind);
        let column = visible
            .iter()
            .map(|c| c.name.width() + 2)
            .max()
            .unwrap_or(0)
            .clamp(8, 40);
        let marker = if app.frame == FrameStyle::Classic {
            ">"
        } else {
            "▸"
        };
        for (i, choice) in visible.iter().enumerate() {
            let active = first + i == menu.selected;
            let name = fit_start(&choice.name, column);
            let pad = column.saturating_sub(name.width());
            let kind = match (mixed, choice.kind) {
                (false, _) => "",
                (true, PathKind::File) => copy::FILE,
                (true, PathKind::Directory) => copy::DIRECTORY,
            };
            let content = Line::from(vec![
                Span::styled(
                    if active {
                        format!("{marker} ")
                    } else {
                        "  ".into()
                    },
                    asking.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{name}{}", " ".repeat(pad)),
                    if active {
                        asking.add_modifier(Modifier::BOLD)
                    } else {
                        Style::new()
                    },
                ),
                Span::styled(kind, if active { Style::new() } else { dim() }),
            ]);
            line(buf, Rect::new(area.x, y, area.width, 1), content);
            y += 1;
        }
        if shown < count && rows > 1 {
            line(
                buf,
                Rect::new(area.x, y, area.width, 1),
                Line::styled(
                    format!("  +{} {}", count - shown, copy::MORE_MATCHES),
                    dim(),
                ),
            );
            y += 1;
        }
    }
    for (text, error) in status {
        if y >= area.bottom() {
            break;
        }
        status_row(app, &text, error, Rect::new(area.x, y, area.width, 1), buf);
        y += 1;
    }
}

/// The slash menu above the bar, as the oracle's `Completion` draws it: a
/// `▸` on the selected row, the names in a column sized to the longest, and
/// each command's hint and description after it, dim unless selected. More
/// commands than rows keep the selected one in view and count the rest.
fn render_menu(app: &State, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let asking = tone_style(Tone::Asking, app.tones);
    if let Some(menu) = app.file_menu() {
        render_file_menu(app, &menu, area, buf);
        return;
    }
    let Some(menu) = app.menu() else {
        if let Some(command) = completion::usage(app.draft.text(), app.catalog()) {
            let text = format!(
                "/{} {}  {}",
                command.name,
                command.hint.unwrap_or_default(),
                command.description
            );
            line(
                buf,
                Rect::new(area.x, area.y, area.width, 1),
                Line::styled(text, dim()),
            );
        }
        return;
    };
    if menu.entries.is_empty() {
        let row = Rect::new(area.x, area.y, area.width, 1);
        status_row(app, copy::NO_COMPLETIONS, false, row, buf);
        return;
    }
    let rows = usize::from(area.height);
    let count = menu.entries.len();
    let shown = if count > rows {
        rows.saturating_sub(1).max(1)
    } else {
        count
    };
    let first = (menu.selected + 1).saturating_sub(shown).min(count - shown);
    let names = menu
        .entries
        .iter()
        .map(|c| c.name.len() + 1)
        .max()
        .unwrap_or(0);
    let column = (names + 2).clamp(8, 40);
    let marker = if app.frame == FrameStyle::Classic {
        ">"
    } else {
        "▸"
    };
    for (i, command) in menu.entries.iter().enumerate().skip(first).take(shown) {
        let active = i == menu.selected;
        let name = format!("/{}", command.name);
        let about = match command.hint {
            Some(hint) => format!("{hint}  {}", command.description),
            None => command.description.to_owned(),
        };
        let pad = column.saturating_sub(name.len());
        let content = Line::from(vec![
            Span::styled(
                if active {
                    format!("{marker} ")
                } else {
                    "  ".into()
                },
                asking.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{name}{}", " ".repeat(pad)),
                if active {
                    asking.add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                },
            ),
            Span::styled(about, if active { Style::new() } else { dim() }),
        ]);
        let y = area.y + (i - first) as u16;
        line(buf, Rect::new(area.x, y, area.width, 1), content);
    }
    if shown < count {
        let y = area.y + shown as u16;
        if y < area.bottom() {
            line(
                buf,
                Rect::new(area.x, y, area.width, 1),
                Line::styled(
                    format!("  +{} {}", count - shown, copy::MORE_MATCHES),
                    dim(),
                ),
            );
        }
    }
}

/// Items a panel above the bar lists before it counts the rest.
const PANEL_ITEMS: usize = 3;

/// Rows a panel above the bar wants for `count` items: its title, up to
/// [`PANEL_ITEMS`] items and one more row, which holds the next item or a
/// count of the rest, and its footer.
fn panel_rows(count: usize) -> u16 {
    if count == 0 {
        return 0;
    }
    let items = count.min(PANEL_ITEMS + 1);
    u16::try_from(items + 2).unwrap_or(u16::MAX)
}

/// The staged images above the bar, as the oracle's attachments panel:
/// `Staged attachments: 2` in blue, a numbered line for each image, and a
/// footer.
fn render_attachments(app: &State, area: Rect, buf: &mut Buffer) {
    let items: Vec<String> = app
        .attachments
        .iter()
        .enumerate()
        .map(|(index, (_, image))| format!("{}. {}", index + 1, image.summary()))
        .collect();
    let title = format!("{}: {}", copy::ATTACHMENTS_TITLE, items.len());
    let style = tone_style(Tone::Asking, app.tones);
    render_panel(&title, style, &items, copy::ATTACHMENTS_HELP, area, buf);
}

/// The prompts the runtime holds above the bar, as the oracle's pending
/// panel: `Pending input` in amber, a line for each prompt with where it
/// will be admitted, `Next step: …`, and the key that sends them now. A
/// prompt of several lines reads as one, its breaks shown as spaces, and is
/// cut to the row; the oracle wraps it instead.
fn render_pending(app: &State, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    let items: Vec<String> = app
        .pending
        .iter()
        .map(|waiting| {
            let target = match waiting.target {
                Target::NextStep => copy::NEXT_STEP,
                Target::NextTurn => copy::NEXT_TURN,
            };
            let text = waiting
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            fit(&format!("{target}: {text}"), width)
        })
        .collect();
    let style = tone_style(Tone::Waiting, app.tones);
    render_panel(copy::PENDING, style, &items, copy::PENDING_HELP, area, buf);
}

/// `text` cut to `width` cells, between graphemes, with `…` standing for
/// what was cut.
pub(crate) fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if used + cells + 1 > width {
            break;
        }
        out.push_str(grapheme);
        used += cells;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// One panel above the bar: a title in its tone, a dim line per item, and a
/// dim footer. Short of rows, the footer and then the items give way, a
/// count standing for those left out, and the title stays to say why.
fn render_panel(
    title: &str,
    style: Style,
    items: &[String],
    footer: &str,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.is_empty() || items.is_empty() {
        return;
    }
    let count = items.len();
    let mut lines = vec![Line::styled(title.to_owned(), style)];
    let room = usize::from(area.height) - 1;
    let with_footer = room >= 2;
    let slots = room - usize::from(with_footer);
    let shown = if count > slots {
        slots.saturating_sub(1)
    } else {
        count
    };
    for item in &items[..shown] {
        lines.push(Line::styled(item.clone(), dim()));
    }
    if shown < count && slots > 0 {
        lines.push(Line::styled(
            format!("+{} {}", count - shown, copy::MORE_LINES),
            dim(),
        ));
    }
    if with_footer {
        lines.push(Line::styled(footer.to_owned(), dim()));
    }
    for (i, content) in lines.into_iter().enumerate() {
        line(
            buf,
            Rect::new(area.x, area.y + i as u16, area.width, 1),
            content,
        );
    }
}

/// A live session's standing row: its running background shells, with
/// the key that opens their list while the composer has focus and the
/// terminal is wide enough for keys.
fn render_jobs_row(app: &State, columns: u16, area: Rect, buf: &mut Buffer) {
    let (left, right) = jobs::standing(
        &app.jobs,
        app.jobs_at,
        app.now,
        usize::from(area.width),
        Look {
            tones: app.tones,
            classic: app.frame == FrameStyle::Classic,
            lit: true,
        },
        // The key draws where any row's key does, so the command takes its
        // cells elsewhere.
        app.focus == Focus::Composer && columns >= HINT_MIN_COLUMNS,
    );
    split_row(buf, columns, area, left, right);
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
fn render_transcript(
    app: &mut State,
    area: Rect,
    gap: Rect,
    buf: &mut Buffer,
    published: Option<usize>,
) {
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
    let range = app.selecting.range();
    let inline_follow = published.is_some() && view.following();
    let screen: Vec<_> = view
        .screen()
        .into_iter()
        .filter(|(at, _)| !inline_follow || at.row >= published.unwrap_or(0))
        .collect();
    let offset = if inline_follow {
        rows.saturating_sub(screen.len() as u16)
    } else {
        0
    };
    for (i, (at, content)) in screen.into_iter().enumerate() {
        let y = area.y + offset + i as u16;
        // The cells a selection covers are drawn reversed, keeping their
        // own colours, as a terminal draws its own selection.
        let selected = range.and_then(|range| line_columns(range, at, &content.to_string()));
        line(buf, Rect::new(area.x, y, width, 1), content);
        if let Some((from, to)) = selected {
            for column in from..to.min(usize::from(width)) {
                buf[(area.x + column as u16, y)]
                    .modifier
                    .insert(Modifier::REVERSED);
            }
        }
    }
    app.transcript_area = Some(Area {
        x: area.x,
        y: area.y + offset,
        width,
        rows: rows - offset,
    })
    .filter(|area| area.rows > 0);
    let thumb = (bar && !inline_follow)
        .then(|| view.thumb(usize::from(rows)))
        .flatten();
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
        // The hint ends where the status line and the agents row under it
        // do, inset from the edge when the composer is boxed.
        let right = if composer::geometry(area.width).boxed {
            INSET
        } else {
            0
        };
        let hint_width = width.min(area.width.saturating_sub(right));
        render_scroll_hint(app, Rect::new(row.x, row.y, hint_width, 1), buf);
    }
}

/// The scroll indicator. While following output, the keys that scroll,
/// right-aligned and quiet. While reading, a pill centred under the text:
/// `↓ 12 lines below · Ctrl+End`, or `↓ New output · …` in yellow once
/// output arrives below. The pill gives up its key, then its count, before
/// it would be cut. Pressing it follows output again.
fn render_scroll_hint(app: &mut State, row: Rect, buf: &mut Buffer) {
    // Whether a selection was copied takes the row for a moment, at the
    // right, apart from the transcript's text.
    if let Some((ok, _)) = app.copied {
        // The green and red of a call's done and failed marks.
        let (text, rgb, ansi) = if ok {
            (copy::COPIED, (0x22, 0xc5, 0x5e), Color::Green)
        } else {
            (copy::COPY_FAILED, (0xef, 0x44, 0x44), Color::Red)
        };
        let cells = (text.len() as u16).min(row.width);
        let style = match app.tones {
            Tones::TrueColor => Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
            Tones::Ansi => Style::new().fg(ansi),
            Tones::None => Style::new(),
        }
        .add_modifier(Modifier::BOLD);
        line(
            buf,
            Rect::new(row.x + row.width - cells, row.y, cells, 1),
            Line::styled(text, style),
        );
        return;
    }
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

fn render_body(app: &mut State, area: Rect, gap: Rect, buf: &mut Buffer, published: Option<usize>) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let classic = app.frame == FrameStyle::Classic;
    let lines = match app.focus {
        Focus::Composer => return render_transcript(app, area, gap, buf, published),
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
        Focus::Jobs => jobs::list(
            &app.jobs,
            app.job_selected.as_deref(),
            app.jobs_at,
            app.now,
            width,
            Look {
                tones: app.tones,
                classic,
                lit: true,
            },
        ),
        Focus::Shell => match app.selected_job() {
            Some(job) => {
                let (lines, most) = jobs::shell(
                    job,
                    app.jobs_at,
                    app.now,
                    width,
                    usize::from(area.height),
                    app.shell_scroll,
                    Look {
                        tones: app.tones,
                        classic,
                        lit: true,
                    },
                );
                app.shell_most = most;
                app.shell_scroll = app.shell_scroll.min(most);
                lines
            }
            None => Vec::new(),
        },
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

/// One agent, read only, as a ledger: its name, a dashed rule, then a label
/// and a value per row, the value wrapped under itself.
fn inspection(agent: &SampleAgent, width: usize, look: Look) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(agent.name, accent().add_modifier(Modifier::BOLD)),
        ]),
        transcript::rule(width, transcript::RAIL, look),
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
    use crate::mention::Candidate;
    use std::time::Duration;

    use crate::state::{
        Activity, COPIED, EDGE_SCROLL, Effect, Mouse, MouseKind, Msg, QUIT_WINDOW, update,
    };

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

    fn key(app: &mut State, key: Key) -> Vec<Effect> {
        update(app, Msg::Key(KeyInput::plain(key)))
    }

    /// Draws `app` as the inline writer does, in the held region's rows at
    /// the bottom of a `height`-row terminal.
    fn draw_inline(app: &mut State, width: u16, height: u16) -> (Vec<String>, Position) {
        let held = inline_height(app, width, height).min(height);
        let mut terminal = Terminal::new(TestBackend::new(width, held)).unwrap();
        terminal.draw(|f| render_inline(app, f, 0, height)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..held)
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

    // Bake: a secret field is drawn masked at every width and frame style,
    // and the draft stays in the dimmed composer.
    #[test]
    fn a_secret_field_is_masked_at_every_width() {
        use crate::dialog::{DialogKind, DialogRequest, FieldDialog};
        use crate::runtime::RuntimeUpdate;
        for style in [FrameStyle::default(), FrameStyle::Classic] {
            for (w, h) in [(12, 8), (20, 10), (40, 12), (80, 24), (200, 40)] {
                let mut app = State::live(style, Tones::None, Vec::new());
                app.draft.type_text("kept");
                update(
                    &mut app,
                    Msg::Runtime(RuntimeUpdate::Dialog(Some(DialogRequest {
                        id: 1,
                        kind: DialogKind::Field(FieldDialog {
                            title: "Sign in".into(),
                            prompt: "API key".into(),
                            secret: true,
                            initial: String::new(),
                            error: None,
                        }),
                    }))),
                );
                let _ = draw(&mut app, w, h);
                if let Some(token) = app.approval_drawn() {
                    update(&mut app, Msg::ApprovalArmed(token));
                }
                update(&mut app, Msg::Paste("sk-fake-secret-value".into()));
                let (rows, _) = draw(&mut app, w, h);
                let screen = rows.join("\n");
                for part in ["sk-fake", "secret", "value"] {
                    assert!(!screen.contains(part), "{w}x{h}: {screen}");
                }
                // Where the field fits whole it holds the paste, masked.
                if app.dialog_ready() {
                    assert!(screen.contains("••••"), "{w}x{h}: {screen}");
                }
            }
        }
    }

    // Bake: a terminal too short for the whole dialog does not hand it the
    // keyboard; once it is tall enough, the next frame does.
    #[test]
    fn a_dialog_cut_short_is_not_handed_the_keyboard() {
        use crate::dialog::{DialogKind, DialogRequest, FieldDialog};
        use crate::runtime::RuntimeUpdate;
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        update(
            &mut app,
            Msg::Runtime(RuntimeUpdate::Dialog(Some(DialogRequest {
                id: 1,
                kind: DialogKind::Field(FieldDialog {
                    title: "Sign in".into(),
                    prompt: "API key".into(),
                    secret: true,
                    initial: String::new(),
                    error: Some("Try again".into()),
                }),
            }))),
        );
        let _ = draw(&mut app, 80, 5);
        assert_eq!(app.approval_drawn(), None);
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows.iter().any(|r| r.contains("Preparing")), "{rows:#?}");
        let token = app.approval_drawn().expect("a handoff");
        update(&mut app, Msg::ApprovalArmed(token));
        assert!(app.dialog_ready());
    }

    // Bake: after a dialog closes, the composer says it is preparing until
    // the handoff is armed, then shows its usual prompt and takes keys.
    #[test]
    fn the_composer_says_it_is_preparing_until_it_has_the_keyboard() {
        use crate::runtime::RuntimeUpdate;
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        update(&mut app, Msg::Runtime(RuntimeUpdate::Dialog(None)));
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows.iter().any(|r| r.contains("Preparing")), "{rows:#?}");
        key(&mut app, Key::Char('x'));
        assert_eq!(app.draft.text(), "");
        let token = app.approval_drawn().expect("a handoff");
        update(&mut app, Msg::ApprovalArmed(token));
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(!rows.iter().any(|r| r.contains("Preparing")), "{rows:#?}");
        key(&mut app, Key::Char('x'));
        assert_eq!(app.draft.text(), "x");
    }

    fn open_list(app: &mut State, list: crate::dialog::ListDialog) {
        use crate::dialog::{DialogKind, DialogRequest};
        use crate::runtime::RuntimeUpdate;
        update(
            app,
            Msg::Runtime(RuntimeUpdate::Dialog(Some(DialogRequest {
                id: 1,
                kind: DialogKind::List(list),
            }))),
        );
    }

    fn arm(app: &mut State, w: u16, h: u16) {
        let _ = draw(app, w, h);
        if let Some(token) = app.approval_drawn() {
            update(app, Msg::ApprovalArmed(token));
        }
    }

    // Pi model-selector.test.ts "keeps the current model marked while
    // browsing", drawn as the TypeScript picker draws a row: the pointer,
    // the label's column, and the `Current` status, which stays on the
    // current model as the selection moves.
    #[test]
    fn a_list_dialog_marks_the_current_item_while_browsing() {
        use crate::dialog::{ListDialog, ListItem};
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        let item = |id: &str| ListItem {
            value: id.into(),
            label: id.into(),
            detail: Some("faux".into()),
            ..ListItem::default()
        };
        open_list(
            &mut app,
            ListDialog {
                title: "Model".into(),
                items: vec![item("current-model"), item("browsed-model")],
                current: Some(0),
                selected: None,
                filterable: true,
                saves_default: true,
                note: None,
                search: None,
            },
        );
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows.iter().any(|r| r.contains("Preparing")));
        let token = app.approval_drawn().expect("a handoff");
        update(&mut app, Msg::ApprovalArmed(token));
        let (rows, _) = draw(&mut app, 80, 24);
        let current = &rows[row_index(&rows, "current-model")];
        assert!(
            current.contains("▸ current-model  Current  faux"),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("Ctrl+S to set as default") && r.contains("1/2"))
        );
        assert!(rows.iter().any(|r| r.contains("> Type to filter")));
        // The sheet is framed and dim-bounded at SHEET_WIDTH or less.
        assert!(rows.iter().any(|r| r.contains('╭') && r.contains('╮')));
        key(&mut app, Key::Down);
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows[row_index(&rows, "current-model")].contains("  current-model  Current  faux"));
        assert!(rows[row_index(&rows, "browsed-model")].contains("▸ browsed-model           faux"));
        assert!(rows.iter().any(|r| r.contains("2/2")));
    }

    // Bake: on a narrow terminal `/share`'s note is cut to six rows, so
    // who can read the gist comes first and is shown whole.
    #[test]
    fn the_share_warning_survives_a_narrow_sheet() {
        let rows = super::sheet_text(crate::copy::SHARE_CONFIRM_NOTE, 40);
        assert_eq!(rows.len(), super::SHEET_TEXT_ROWS);
        assert!(rows[5].ends_with('…'), "the note is cut: {rows:#?}");
        assert!(
            rows.join(" ").starts_with(
                "Anyone with the gist's link can read it: a secret gist is unlisted, not private."
            ),
            "{rows:#?}"
        );
    }

    // Bake: a list's note and a field's prompt wrap to the sheet's width,
    // their lines kept, so a notice such as what `/share` uploads is read
    // whole; past six rows the last ends in `…`.
    #[test]
    fn notes_and_prompts_wrap_whole_within_the_sheet() {
        use crate::dialog::{DialogKind, DialogRequest, FieldDialog, ListDialog, ListItem};
        use crate::runtime::RuntimeUpdate;
        let note = "The gist holds this session as a web page, and anyone with its link can read every word of it until it is deleted.\nSecond line.";
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        open_list(
            &mut app,
            ListDialog {
                title: "Share?".into(),
                items: vec![ListItem {
                    value: "no".into(),
                    label: "No".into(),
                    ..ListItem::default()
                }],
                current: None,
                selected: None,
                filterable: false,
                saves_default: false,
                note: Some(note.into()),
                search: None,
            },
        );
        arm(&mut app, 50, 24);
        let (rows, _) = draw(&mut app, 50, 24);
        let screen = rows.join("\n");
        for word in [
            "web page,",
            "read every",
            "word of it",
            "deleted.",
            "Second line.",
        ] {
            assert!(screen.contains(word), "{word}: {screen}");
        }
        assert!(app.dialog_ready());
        let long = vec!["word"; 200].join(" ");
        update(
            &mut app,
            Msg::Runtime(RuntimeUpdate::Dialog(Some(DialogRequest {
                id: 2,
                kind: DialogKind::Field(FieldDialog {
                    title: "Report".into(),
                    prompt: long.clone(),
                    secret: false,
                    initial: String::new(),
                    error: None,
                }),
            }))),
        );
        assert_eq!(super::sheet_text(&long, 50).len(), super::SHEET_TEXT_ROWS);
        assert!(super::sheet_text(&long, 50)[5].ends_with('…'));
        arm(&mut app, 50, 24);
        let (rows, _) = draw(&mut app, 50, 24);
        let prompt_rows = rows.iter().filter(|row| row.contains("word word")).count();
        assert_eq!(prompt_rows, super::SHEET_TEXT_ROWS, "{rows:#?}");
        assert!(app.dialog_ready());
    }

    /// A session search for render tests: `bad` is an invalid pattern,
    /// and otherwise a row is listed when its label holds the query.
    struct LabelSearch(Vec<(&'static str, bool)>);

    impl crate::dialog::SessionFilter for LabelSearch {
        fn search(
            &self,
            query: &str,
            _: crate::dialog::SearchOrder,
            named_only: bool,
        ) -> crate::dialog::SearchResult {
            if query == "bad" {
                return crate::dialog::SearchResult {
                    rows: Vec::new(),
                    error: Some(crate::dialog::SearchError::InvalidRegex(
                        "look-around, including look-ahead and look-behind, is not supported"
                            .into(),
                    )),
                };
            }
            crate::dialog::SearchResult {
                rows: (0..self.0.len())
                    .filter(|&i| (!named_only || self.0[i].1) && self.0[i].0.contains(query))
                    .collect(),
                error: None,
            }
        }
    }

    // Pi session-selector.ts's header and hints, drawn in the shared sheet:
    // the order and named state with their keys, the search syntax in the
    // empty search row, the current mark apart from the selection, and an
    // invalid pattern's reason, all legible in a 42-column terminal.
    #[test]
    fn the_session_sheet_shows_its_search_state_at_42_columns() {
        use crate::dialog::{ListDialog, ListItem, SessionSearch};
        let labels = vec![("Release prep", true), ("Fix flaky test", false)];
        for width in [42u16, 80] {
            let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
            open_list(
                &mut app,
                ListDialog {
                    title: "Resume session · 2 saved here".into(),
                    items: labels
                        .iter()
                        .map(|(label, _)| ListItem {
                            value: (*label).into(),
                            label: (*label).into(),
                            detail: Some("2h ago · 01a13e27".into()),
                            ..ListItem::default()
                        })
                        .collect(),
                    current: Some(0),
                    selected: None,
                    filterable: true,
                    saves_default: false,
                    note: None,
                    search: Some(SessionSearch::new(LabelSearch(labels.clone()))),
                },
            );
            arm(&mut app, width, 24);
            let (rows, _) = draw(&mut app, width, 24);
            assert!(
                rows.iter()
                    .any(|r| r.contains("^S sort: Recent · ^N named: off")),
                "{rows:#?}"
            );
            assert!(
                rows.iter()
                    .any(|r| r.contains("> Search · re:<regex> · \"phrase\"")),
                "{rows:#?}"
            );
            assert!(
                rows.iter().any(|r| r.contains("▸ Release prep")),
                "{rows:#?}"
            );
            key(&mut app, Key::Down);
            let (rows, _) = draw(&mut app, width, 24);
            assert!(rows[row_index(&rows, "Release prep")].contains("Current"));
            assert!(
                rows.iter().any(|r| r.contains("▸ Fix flaky test")),
                "{rows:#?}"
            );
            update(
                &mut app,
                Msg::Key(KeyInput {
                    key: Key::Char('s'),
                    mods: Mods::CTRL,
                }),
            );
            update(
                &mut app,
                Msg::Key(KeyInput {
                    key: Key::Char('n'),
                    mods: Mods::CTRL,
                }),
            );
            let (rows, _) = draw(&mut app, width, 24);
            assert!(
                rows.iter()
                    .any(|r| r.contains("^S sort: Relevance · ^N named: on")),
                "{rows:#?}"
            );
            assert!(
                !rows.iter().any(|r| r.contains("Fix flaky test")),
                "{rows:#?}"
            );
            for c in "bad".chars() {
                key(&mut app, Key::Char(c));
            }
            let (rows, _) = draw(&mut app, width, 24);
            assert!(
                rows.iter()
                    .any(|r| r.contains("Invalid regex: look-around")),
                "{rows:#?}"
            );
            assert!(
                rows.iter().any(|r| r.contains("No matching sessions")),
                "{rows:#?}"
            );
            assert!(rows.iter().all(|r| r.chars().count() == usize::from(width)));
        }
    }

    /// Thirty models of two providers, readable names sharing prefixes.
    fn models() -> crate::dialog::ListDialog {
        use crate::dialog::{ListDialog, ListItem};
        let items = (1..=30)
            .map(|n| {
                let provider = if n <= 15 { "alpha" } else { "beta" };
                let id = format!("claude-sonnet-4-5-2025{n:04}");
                ListItem {
                    value: format!("{provider}/{id}"),
                    label: format!("Claude Sonnet 4.5 ({n})"),
                    detail: Some(format!("{provider}/{id}")),
                    group: Some(provider.into()),
                    facts: vec!["200k".into(), "think".into()],
                    about: Some(format!("{provider}/{id}")),
                }
            })
            .collect();
        ListDialog {
            title: "Model · 30 models from alpha, beta".into(),
            items,
            current: Some(20),
            selected: None,
            filterable: true,
            saves_default: true,
            note: None,
            search: None,
        }
    }

    // TS sheet.tsx `pageStart` and picker.tsx: a list longer than its rows
    // scrolls only to keep the selection in view, each edge says how many
    // rows it hides, groups keep their headings, and the selected row and
    // its full route stay on screen at every size that hands it the keys.
    #[test]
    fn a_long_list_follows_its_selection_at_every_size() {
        for inline in [false, true] {
            for (w, h) in [
                (80, 24),
                (42, 24),
                (42, 14),
                (120, 40),
                (24, 12),
                (22, 14),
                (16, 10),
            ] {
                let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
                open_list(&mut app, models());
                arm(&mut app, w, h);
                let mut seen = Vec::new();
                for step in 0..32 {
                    let (rows, _) = if inline {
                        draw_inline(&mut app, w, h)
                    } else {
                        draw(&mut app, w, h)
                    };
                    let screen = rows.join("\n");
                    if app.dialog_ready() {
                        let selected = app.dialog.as_ref().expect("dialog").selected;
                        let n = selected + 1;
                        // The selected row is drawn, pointed at.
                        assert!(
                            rows.iter().any(|r| r.contains('▸')),
                            "{w}x{h} step {step}: {screen}"
                        );
                        if w >= 42 {
                            let route = format!("claude-sonnet-4-5-2025{n:04}");
                            assert!(
                                screen.contains(&route),
                                "{w}x{h} step {step} {route}: {screen}"
                            );
                            assert!(
                                screen.contains(&format!("{}/30", selected + 1)),
                                "{w}x{h}: {screen}"
                            );
                        }
                        for row in &rows {
                            assert!(row.width() <= usize::from(w), "{w}x{h}: {row}");
                        }
                        // However narrow, the armed sheet says how to cancel.
                        assert!(
                            rows.iter()
                                .any(|r| r.contains('│') && r.contains("Esc") && !r.contains('❯')),
                            "{w}x{h}: {screen}"
                        );
                        seen.push(selected);
                    }
                    key(&mut app, Key::Down);
                }
                if w >= 24 {
                    assert!(!seen.is_empty(), "{w}x{h} never ready");
                }
            }
        }
    }

    // TS picker.tsx: the edges count hidden rows; a filter keeps the
    // sheet's height and matches a model's exact route.
    #[test]
    fn a_filtered_list_keeps_its_height_and_matches_routes() {
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        open_list(&mut app, models());
        arm(&mut app, 80, 30);
        let (rows, _) = draw(&mut app, 80, 30);
        let height = rows.iter().filter(|r| r.contains('│')).count();
        assert!(rows.iter().any(|r| r.contains("more")), "{rows:#?}");
        for c in "beta/claude-sonnet-4-5-20250021".chars() {
            key(&mut app, Key::Char(c));
        }
        let (rows, _) = draw(&mut app, 80, 30);
        assert_eq!(rows.iter().filter(|r| r.contains('│')).count(), height);
        assert!(
            rows.iter().any(|r| r.contains("▸ Claude Sonnet 4.5 (21)")),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|r| r.contains("1/1")));
        for c in "zz".chars() {
            key(&mut app, Key::Char(c));
        }
        let (rows, _) = draw(&mut app, 80, 30);
        assert!(
            rows.iter().any(|r| r.contains("No matching choices")),
            "{rows:#?}"
        );
    }

    // Bake: a sheet too narrow to show its selection is never handed the
    // keyboard, so Enter cannot choose what was not seen; Esc still cancels.
    #[test]
    fn an_unusably_narrow_sheet_is_not_handed_the_keyboard() {
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        open_list(&mut app, models());
        arm(&mut app, 8, 30);
        assert!(!app.dialog_ready());
        assert!(key(&mut app, Key::Enter).is_empty());
        let effects = key(&mut app, Key::Esc);
        assert!(matches!(
            &effects[..],
            [Effect::Dialog {
                answer: crate::dialog::DialogAnswer::Cancel,
                ..
            }]
        ));
    }

    // TS sheet.tsx `sheetRows`: an information sheet wraps a long value
    // under its own column, so a long session path and a Unicode name are
    // shown whole, scrolling where they do not fit.
    #[test]
    fn an_info_sheet_wraps_long_values_and_scrolls() {
        use crate::dialog::{DialogKind, DialogRequest, InfoDialog, InfoLine};
        use crate::runtime::RuntimeUpdate;
        let path = format!(
            "/tmp/{}/2026-10-15T06-02-13-759Z_01a13e27-6d7f-7128-952d-5840b7da8c4b.jsonl",
            "很长的目录".repeat(4)
        );
        let mut lines = vec![
            InfoLine::Field {
                label: "Name".into(),
                value: "日本語のセッション 🚀".into(),
            },
            InfoLine::Field {
                label: "File".into(),
                value: path.clone(),
            },
        ];
        for n in 0..20 {
            lines.push(InfoLine::Field {
                label: format!("Row {n}"),
                value: n.to_string(),
            });
        }
        for (w, h) in [(80, 24), (42, 16), (30, 12)] {
            let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
            update(
                &mut app,
                Msg::Runtime(RuntimeUpdate::Dialog(Some(DialogRequest {
                    id: 4,
                    kind: DialogKind::Info(InfoDialog {
                        title: "Session info".into(),
                        lines: lines.clone(),
                    }),
                }))),
            );
            arm(&mut app, w, h);
            assert!(app.dialog_ready(), "{w}x{h}");
            // Each body row by its place in the whole sheet, as it scrolls.
            let mut seen = std::collections::BTreeMap::new();
            for _ in 0..40 {
                let (rows, _) = draw(&mut app, w, h);
                for row in &rows {
                    assert!(row.width() <= usize::from(w), "{w}x{h}: {row}");
                }
                let inside: Vec<String> = rows
                    .iter()
                    .filter(|row| row.matches('│').count() == 2)
                    .map(|row| row.split('│').nth(1).unwrap_or("").trim().to_owned())
                    .collect();
                let top = app.dialog.as_ref().expect("dialog").top;
                // Inside the frame: the title, the body, then the keys.
                for (i, row) in inside
                    .iter()
                    .skip(1)
                    .take(inside.len().saturating_sub(2))
                    .enumerate()
                {
                    seen.insert(top + i, row.clone());
                }
                key(&mut app, Key::Down);
            }
            let text: String = seen.values().cloned().collect();
            let joined: String = text.split_whitespace().collect();
            let wanted: String = path.split_whitespace().collect();
            assert!(
                joined.contains(&wanted),
                "{w}x{h}: the whole path was never shown"
            );
            assert!(joined.contains("日本語のセッション🚀"), "{w}x{h}");
            assert!(joined.contains("Row19"), "{w}x{h}");
            // Enter closes it, as Esc does.
            let effects = key(&mut app, Key::Enter);
            assert!(matches!(
                &effects[..],
                [Effect::Dialog {
                    answer: crate::dialog::DialogAnswer::Cancel,
                    ..
                }]
            ));
        }
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
        app.draft.type_text("/keep me");
        key(&mut app, Key::Enter);
        let (rows, cursor) = draw(&mut app, 80, 24);
        let notice = row_index(&rows, "Commands are not available");
        let bar = row_index(&rows, "no model");
        let prompt = row_index(&rows, "❯ /keep me");
        assert!(notice < bar && bar + 2 == prompt);
        assert_eq!(cursor, Position::new(TEXT_X + 8, prompt as u16));
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
        // The body is a ledger under the agent's name and a dashed rule.
        let name = rows
            .iter()
            .position(|r| r.trim_end() == "  Sample explorer")
            .unwrap();
        assert!(rows[name + 1].starts_with("  ---"));
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
        app.activity = Some(Activity {
            kind: ActivityKind::Turn,
            word,
            started: Duration::from_secs(1),
            runtime: false,
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
        app.notice = Some(Notice::NoCommands);
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

    /// Each screen row's reversed cells, as text; empty where none are.
    fn reversed(app: &mut State, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .filter(|&x| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect()
            })
            .collect()
    }

    /// The screen row holding `needle`, and the column it starts at.
    fn find(rows: &[String], needle: &str) -> (u16, u16) {
        let row = row_index(rows, needle);
        let line = &rows[row];
        let column = line[..line.find(needle).unwrap()].chars().count();
        (row as u16, column as u16)
    }

    #[test]
    fn a_drag_in_the_transcript_selects_and_reverses_its_cells() {
        let mut app = State::default();
        let (rows, _) = draw(&mut app, 80, 24);
        let (y, x) = find(&rows, "before splitting.");
        update(&mut app, mouse(MouseKind::Down, x, y, 0));
        update(&mut app, mouse(MouseKind::Drag, x + 5, y, 10));
        update(&mut app, mouse(MouseKind::Up, x + 5, y, 20));
        let marked = reversed(&mut app, 80, 24);
        // A dragged end covers its own cell.
        assert_eq!(marked[usize::from(y)], "before");
        assert!(
            marked
                .iter()
                .enumerate()
                .all(|(i, m)| i == usize::from(y) || m.is_empty())
        );
        // A click selects nothing and clears what was selected.
        update(&mut app, mouse(MouseKind::Down, x, y, 1000));
        update(&mut app, mouse(MouseKind::Up, x, y, 1010));
        assert!(app.selecting.range().is_none());
    }

    #[test]
    fn a_release_copies_the_selection_and_the_indicator_says_so() {
        let mut app = State::default();
        let (rows, _) = draw(&mut app, 80, 24);
        let (y, x) = find(&rows, "before splitting.");
        // Dragged from the line above to the middle of this one: each line
        // as drawn, cut to its cells and trimmed at its end.
        // The scrollbar's column is not the transcript's, so it is not copied.
        let above: String = rows[usize::from(y) - 1].chars().take(79).collect();
        let above = above.trim_end();
        update(&mut app, mouse(MouseKind::Down, 0, y - 1, 0));
        update(&mut app, mouse(MouseKind::Drag, x + 5, y, 10));
        let copied = update(&mut app, mouse(MouseKind::Up, x + 5, y, 20));
        let text = format!("{above}\n  rejects it before");
        assert_eq!(copied, [Effect::Copy(text)]);
        // A click copies nothing.
        update(&mut app, mouse(MouseKind::Down, x, y, 1000));
        assert!(update(&mut app, mouse(MouseKind::Up, x, y, 1010)).is_empty());
        // The indicator says it was copied, at the right, then goes back.
        update(&mut app, Msg::Copied(true));
        let (rows, _) = draw(&mut app, 80, 24);
        let bar = row_index(&rows, "no model");
        assert!(
            rows[bar - 1].trim_end().ends_with("Copied"),
            "{:?}",
            rows[bar - 1]
        );
        assert_eq!(app.next_change(), Some(COPIED));
        update(&mut app, Msg::Tick(COPIED));
        update(&mut app, Msg::Copied(false));
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows[bar - 1].contains("Copy failed"));
        update(&mut app, Msg::Tick(COPIED * 2));
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows[bar - 1].contains("PgUp scroll"));
    }

    #[test]
    fn a_double_click_takes_a_word_and_a_triple_its_line() {
        let mut app = State::default();
        let (rows, _) = draw(&mut app, 80, 24);
        let (y, x) = find(&rows, "splitting.");
        let click = |app: &mut State, at: u64| {
            update(app, mouse(MouseKind::Down, x + 2, y, at));
            update(app, mouse(MouseKind::Up, x + 2, y, at + 5));
        };
        click(&mut app, 0);
        click(&mut app, 100);
        assert_eq!(reversed(&mut app, 80, 24)[usize::from(y)], "splitting");
        click(&mut app, 200);
        // A line runs from its first cell to the end of its text.
        let line = reversed(&mut app, 80, 24)[usize::from(y)].clone();
        assert_eq!(line, "  rejects it before splitting.");
        // Too slow, it starts over as a click.
        click(&mut app, 2000);
        click(&mut app, 2600);
        assert!(app.selecting.range().is_none());
    }

    #[test]
    fn esc_and_a_new_width_drop_the_selection_before_anything_else() {
        let mut app = State::default();
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('t'), Mods::CTRL)),
        );
        let (rows, _) = draw(&mut app, 80, 24);
        let (y, x) = find(&rows, "before splitting.");
        let select = |app: &mut State| {
            update(app, mouse(MouseKind::Down, x, y, 0));
            update(app, mouse(MouseKind::Drag, x + 3, y, 10));
            update(app, mouse(MouseKind::Up, x + 3, y, 20));
        };
        select(&mut app);
        assert!(app.selecting.range().is_some());
        key(&mut app, Key::Esc);
        assert!(app.selecting.range().is_none());
        // The sample turn still runs: Esc only dropped the selection.
        assert!(app.activity.is_some());
        select(&mut app);
        update(&mut app, Msg::Resize { cols: 80, rows: 24 });
        update(&mut app, Msg::Resize { cols: 80, rows: 30 });
        assert!(app.selecting.range().is_some(), "a new height keeps it");
        update(
            &mut app,
            Msg::Resize {
                cols: 100,
                rows: 30,
            },
        );
        assert!(app.selecting.range().is_none());
    }

    #[test]
    fn a_drag_held_at_the_top_scrolls_the_selection_on() {
        let mut app = State::default();
        let (rows, _) = draw(&mut app, 80, 24);
        let (y, x) = find(&rows, "before splitting.");
        update(&mut app, mouse(MouseKind::Down, x, y, 0));
        update(&mut app, mouse(MouseKind::Drag, 4, 0, 10));
        assert!(app.transcript.following());
        // The loop wakes for the edge, and each step scrolls a line.
        assert_eq!(
            app.next_change(),
            Some(EDGE_SCROLL + Duration::from_millis(10))
        );
        update(
            &mut app,
            Msg::Tick(Duration::from_millis(10) + EDGE_SCROLL * 3),
        );
        assert!(!app.transcript.following());
        let top = app.transcript.top();
        assert_eq!(app.selecting.range().map(|r| r.start.at), Some(top));
        // Released, the edge stops.
        update(&mut app, mouse(MouseKind::Up, 4, 0, 200));
        assert_eq!(app.next_change(), None);
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
        // Following: the scroll keys sit right-aligned on the row above the
        // bar, ending in the status line's column.
        let hint = rows[bar - 1].trim_end();
        assert!(hint.ends_with("Wheel/PgUp scroll · Ctrl+↑ prompts"));
        assert_eq!(hint.chars().count(), rows[bar].trim_end().chars().count());
        key(&mut app, Key::PageUp);
        let (rows, _) = draw(&mut app, 80, 24);
        let pill = &rows[bar - 1];
        let below = app.transcript.total() - app.transcript.offset() - app.transcript.height;
        let text = format!(" ↓ {below} lines below · Ctrl+End ");
        let start = pill.find(&text).expect(pill);
        // Centred on the row the status line spans.
        let left = pill[..start].chars().count();
        let right = 78 - left - text.chars().count();
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
    fn staged_images_are_listed_in_a_panel_above_the_bar() {
        use crate::paste::Image;
        use crate::state::ImageSource;
        let mut app = State::default();
        for name in ["a.png", "b.png", "c.png", "d.png", "e.png"] {
            let image = Image {
                name: name.into(),
                media_type: "image/png",
                bytes: 10,
                size: Some((2, 1)),
                data: crate::paste::ImageData::default(),
            };
            update(
                &mut app,
                Msg::ImageRead {
                    source: ImageSource::Clipboard,
                    image: Some(image),
                },
            );
        }
        let (rows, _) = draw(&mut app, 80, 24);
        let title = row_index(&rows, "Staged attachments: 5");
        assert_eq!(
            rows[title + 1].trim_end(),
            "  1. a.png · image/png · 10 B · 2×1"
        );
        assert_eq!(rows[title + 4].trim_end(), "  +2 more lines");
        assert!(rows[title + 5].contains("Erase a placeholder"));
        assert_eq!(title + 6, row_index(&rows, "no model"));
        // Erasing every placeholder takes the panel away.
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('u'), Mods::CTRL)),
        );
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(!rows.iter().any(|r| r.contains("Staged attachments")));
    }

    fn found(app: &mut State, query: &str, found: Result<Vec<Candidate>, String>) {
        update(
            app,
            Msg::FilesFound {
                query: query.into(),
                found,
            },
        );
    }

    fn candidate(path: &str, kind: PathKind) -> Candidate {
        Candidate {
            path: path.into(),
            kind,
        }
    }

    #[test]
    fn the_file_menu_lists_mentions_and_says_when_it_looks_or_fails() {
        let mut app = State::default();
        key(&mut app, Key::Char('@'));
        let (rows, _) = draw(&mut app, 80, 30);
        let bar = row_index(&rows, "no model");
        assert_eq!(rows[bar - 1].trim_end(), "    Finding paths…");
        // One kind: no kind column. The box's edge names Tab.
        let files = (0..10)
            .map(|i| candidate(&format!("f{i}.md"), PathKind::File))
            .collect();
        found(&mut app, "", Ok(files));
        let (rows, _) = draw(&mut app, 80, 30);
        let first = row_index(&rows, "@f0.md");
        assert_eq!(rows[first].trim_end(), "  ▸ @f0.md");
        assert_eq!(
            rows[first + 8].trim_end(),
            "    +2 more, keep typing to narrow"
        );
        assert_eq!(first + 9, bar);
        assert!(rows.iter().any(|r| r.contains("Tab completes")));
        // Mixed kinds name each; a long path keeps its end.
        let long = format!("{}/name.rs", "deep".repeat(12));
        found(
            &mut app,
            "",
            Ok(vec![
                candidate("src", PathKind::Directory),
                candidate(&long, PathKind::File),
            ]),
        );
        let (rows, _) = draw(&mut app, 80, 30);
        assert!(rows[bar - 2].starts_with("  ▸ @src/"), "{rows:#?}");
        assert!(rows[bar - 2].trim_end().ends_with("Directory"));
        assert!(rows[bar - 1].starts_with("    …"));
        assert!(rows[bar - 1].contains("deep/name.rsFile"));
        // Nothing found, and a failure, each take a row.
        found(&mut app, "", Err("denied".into()));
        let (rows, _) = draw(&mut app, 80, 30);
        assert_eq!(rows[bar - 2].trim_end(), "    No matching paths");
        assert_eq!(
            rows[bar - 1].trim_end(),
            "    File discovery failed: denied"
        );
        assert!(!rows.iter().any(|r| r.contains("Tab completes")));
    }

    #[test]
    fn the_slash_menu_sits_above_the_bar_and_keeps_its_selection_in_view() {
        let mut app = State::default();
        key(&mut app, Key::Char('/'));
        let (rows, _) = draw(&mut app, 80, 30);
        let bar = row_index(&rows, "no model");
        // Eight of thirteen commands, the frequent ones first, then a count.
        let first = row_index(&rows, "/model");
        assert_eq!(first + 9, bar);
        assert!(rows[first].starts_with("  ▸ /model     [provider/model [effort]]  Choose model"));
        assert!(rows[first + 1].starts_with("    /resume    Browse sessions"));
        assert_eq!(
            rows[first + 8].trim_end(),
            "    +5 more, keep typing to narrow"
        );
        // The box's edge names the menu's key.
        assert!(rows.iter().any(|r| r.contains("Tab completes")));
        // Up from the first wraps to the last, which scrolls into view.
        key(&mut app, Key::Up);
        let (rows, _) = draw(&mut app, 80, 30);
        assert!(rows[bar - 2].starts_with("  ▸ /thinking"));
        assert!(!rows.iter().any(|r| r.contains("/model")));
        // Nothing matching says so in one row, past the rail.
        key(&mut app, Key::Char('z'));
        let (rows, _) = draw(&mut app, 80, 30);
        assert_eq!(rows[bar - 1].trim_end(), "    No matching commands");
        assert!(!rows.iter().any(|r| r.contains("Tab completes")));
        // A typed command with a hint shows its usage once the menu closes.
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('u'), Mods::CTRL)),
        );
        for c in "/goal ".chars() {
            key(&mut app, Key::Char(c));
        }
        let (rows, _) = draw(&mut app, 80, 30);
        assert_eq!(
            rows[bar - 1].trim_end(),
            "  /goal [objective|clear]  Set or view the goal for a long-running task"
        );
    }

    #[test]
    fn an_armed_quit_asks_for_a_second_press_above_the_bar() {
        let mut app = State::default();
        app.notice = Some(Notice::NoCommands);
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
    fn pending_input_sits_above_the_attachments_and_cuts_long_prompts() {
        use crate::runtime::{Pending, RuntimeUpdate, Target};
        let mut app = State::default();
        let waiting = |text: &str, target| Pending {
            text: text.into(),
            target,
        };
        update(
            &mut app,
            Msg::Runtime(RuntimeUpdate::Pending(vec![
                waiting("first\nsecond", Target::NextStep),
                waiting(&"long ".repeat(30), Target::NextTurn),
            ])),
        );
        app.attachments.push((
            1,
            crate::paste::Image {
                name: "a.png".into(),
                media_type: "image/png",
                bytes: 1,
                size: None,
                data: crate::paste::ImageData::default(),
            },
        ));
        let (rows, _) = draw(&mut app, 60, 24);
        let title = row_index(&rows, "Pending input");
        assert!(rows[title].starts_with("  Pending input"));
        assert_eq!(rows[title + 1].trim_end(), "  Next step: first second");
        assert!(rows[title + 2].starts_with("  Next turn: long long"));
        assert!(rows[title + 2].trim_end().ends_with('…'));
        assert!(rows[title + 2].width() <= 60);
        assert_eq!(rows[title + 3].trim_end(), "  Alt+↑ sends it now");
        assert_eq!(title + 4, row_index(&rows, "Staged attachments: 1"));
        // With nothing waiting, the panel gives its rows back.
        update(&mut app, Msg::Runtime(RuntimeUpdate::Pending(Vec::new())));
        let (rows, _) = draw(&mut app, 60, 24);
        assert!(!rows.iter().any(|r| r.contains("Pending input")));
    }

    #[test]
    fn inline_keeps_fullscreen_composer_rows_and_absolute_mouse_geometry() {
        use ratatui_core::terminal::{TerminalOptions, Viewport};
        for width in [1, 12, 40, 80] {
            for physical_height in [1, 4, 8, 24, 40] {
                for draft_rows in [1, 2, 5, 12] {
                    let mut app = State::default();
                    app.draft.paste(&vec!["x"; draft_rows].join("\n"));
                    let height = inline_height(&app, width, physical_height);
                    let expected =
                        layout::plan(physical_height, needs(&app, width, physical_height)).composer;
                    let published = app.settled_rows();
                    let mut terminal = Terminal::with_options(
                        TestBackend::new(width, physical_height),
                        TerminalOptions {
                            viewport: Viewport::Fixed(Rect::new(
                                0,
                                physical_height - height,
                                width,
                                height,
                            )),
                        },
                    )
                    .unwrap();
                    terminal
                        .draw(|frame| render_inline(&mut app, frame, published, physical_height))
                        .unwrap();
                    let spot = app.draft_spot.unwrap();
                    assert_eq!(
                        spot.rows, expected,
                        "{width}x{physical_height}, {draft_rows} draft rows"
                    );
                    assert!(spot.top_row >= physical_height - height);
                    let expected_caret =
                        editor::layout(app.draft.text(), app.draft.caret(), spot.width).rows
                            [spot.first]
                            .start;
                    update(
                        &mut app,
                        mouse(MouseKind::Down, spot.text_x, spot.top_row, 0),
                    );
                    assert_eq!(app.draft.caret(), expected_caret);
                }
            }
        }
    }

    #[test]
    fn inline_selection_copies_the_displayed_live_row_after_padding() {
        use ratatui_core::terminal::{TerminalOptions, Viewport};
        let mut app = State::default();
        let published = app.settled_rows();
        app.transcript
            .set_live(vec![transcript::Row::Answer("live words".into())]);
        let height = (inline_height(&app, 80, 24) + 3).min(24);
        let mut terminal = Terminal::with_options(
            TestBackend::new(80, 24),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 24 - height, 80, height)),
            },
        )
        .unwrap();
        terminal
            .draw(|frame| render_inline(&mut app, frame, published, 24))
            .unwrap();
        let area = app.transcript_area.unwrap();
        let displayed: Vec<_> = app
            .transcript
            .screen()
            .into_iter()
            .filter(|(at, _)| at.row >= published)
            .collect();
        let row = displayed
            .iter()
            .position(|(_, text)| text.to_string().contains("live words"))
            .unwrap();
        let column =
            u16::try_from(displayed[row].1.to_string().find("live words").unwrap()).unwrap();
        let y = area.y + row as u16;
        update(&mut app, mouse(MouseKind::Down, column, y, 0));
        update(&mut app, mouse(MouseKind::Drag, column + 10, y, 10));
        assert_eq!(
            update(&mut app, mouse(MouseKind::Up, column + 10, y, 20)),
            vec![Effect::Copy("live words".into())]
        );
    }

    #[test]
    fn fit_cuts_between_graphemes_and_marks_the_cut() {
        assert_eq!(fit("abc", 3), "abc");
        assert_eq!(fit("abcd", 3), "ab…");
        assert_eq!(fit("界界界", 4), "界…");
        assert_eq!(fit("e\u{301}e\u{301}e", 3), "e\u{301}e\u{301}e");
        assert_eq!(fit("e\u{301}e\u{301}e", 2), "e\u{301}…");
        assert_eq!(fit("ab", 0), "");
    }

    #[test]
    fn a_notice_sits_between_the_pill_and_the_bar() {
        let mut app = State::default();
        draw(&mut app, 80, 24);
        key(&mut app, Key::PageUp);
        app.notice = Some(Notice::NoCommands);
        let (rows, _) = draw(&mut app, 80, 24);
        let bar = row_index(&rows, "no model");
        assert!(rows[bar - 1].contains("Commands are not available"));
        assert!(rows[bar - 2].contains("lines below · Ctrl+End"));
    }

    // ---- Approvals ----

    fn asking(reason: &str, arguments: &str) -> State {
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        update(&mut app, Msg::Key(KeyInput::plain(Key::Char('d'))));
        update(
            &mut app,
            Msg::Runtime(crate::runtime::RuntimeUpdate::Approvals(vec![
                crate::runtime::Approval {
                    generation: 4,
                    tool: "bash\x1b[31m".into(),
                    call_id: "call\x07-4".into(),
                    parent_call_id: None,
                    reason: reason.into(),
                    arguments: arguments.into(),
                },
            ])),
        );
        app
    }

    #[test]
    fn the_approval_panel_shows_the_call_its_reason_and_arguments_without_escapes() {
        let mut app = asking(
            "escalate sandbox to danger-full-access: \x1b]52;c;aGk=\x07fetch",
            "command: curl -fsS https://example.com",
        );
        let (rows, _) = draw(&mut app, 80, 24);
        let screen = rows.join("\n");
        assert!(
            !screen.contains('\x1b') && !screen.contains('\x07'),
            "{screen}"
        );
        assert!(
            screen.contains("Approval required: bash[31m  call-4"),
            "{screen}"
        );
        assert!(screen.contains("escalate sandbox to danger-full-access: ]52;c;aGk=fetch"));
        assert!(screen.contains("Arguments:"));
        assert!(screen.contains("command: curl -fsS https://example.com"));
        // Not armed yet: no key but Esc is offered.
        assert!(screen.contains(copy::APPROVAL_PREPARING), "{screen}");
        assert!(!screen.contains(copy::APPROVAL_KEYS), "{screen}");
        // The draft stays, dimmed, below the panel.
        assert!(screen.contains("❯ d"), "{screen}");
        assert_eq!(app.approval_drawn(), Some(1), "a new showing");
        // The preparing frame counts as shown; once armed, the keys show.
        update(&mut app, Msg::ApprovalArmed(1));
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows.join("\n").contains(copy::APPROVAL_KEYS));
        assert_eq!(app.approval_drawn(), None, "still the same showing");
        app.draft = crate::editor::Draft::default();
        update(
            &mut app,
            Msg::Runtime(crate::runtime::RuntimeUpdate::TurnStarted {
                seed: "approval".into(),
            }),
        );
        let (rows, _) = draw(&mut app, 80, 24);
        let screen = rows.join("\n");
        assert!(screen.contains("Approval pending"));
        assert!(screen.contains(copy::HINT_APPROVAL));
        assert!(!screen.contains("Enter steers") && !screen.contains(copy::HINT_INTERRUPT));
    }

    #[test]
    fn an_approval_a_script_asked_for_names_its_codemode_call() {
        let mut app = State::live(FrameStyle::default(), Tones::None, Vec::new());
        let approval = |parent: &str| crate::runtime::Approval {
            generation: 1,
            tool: "bash".into(),
            call_id: format!("{parent}/2"),
            parent_call_id: Some(parent.into()),
            reason: "escalate sandbox to danger-full-access: needs the network".into(),
            arguments: "command: curl -fsS https://example.com".into(),
        };
        update(
            &mut app,
            Msg::Runtime(crate::runtime::RuntimeUpdate::Approvals(vec![approval(
                "call_1",
            )])),
        );
        let (rows, _) = draw(&mut app, 80, 24);
        let screen = rows.join("\n");
        assert!(
            screen.contains("Approval required: bash  call_1/2  from codemode call_1"),
            "{screen}"
        );
        // A long id is cut, since the call's own id holds all of it.
        update(
            &mut app,
            Msg::Runtime(crate::runtime::RuntimeUpdate::Approvals(vec![approval(
                "call_abcdefghijklmnop",
            )])),
        );
        let (rows, _) = draw(&mut app, 100, 24);
        let screen = rows.join("\n");
        assert!(
            screen.contains("call_abcdefghijklmnop/2  from codemode call_abcdef…"),
            "{screen}"
        );
        // With others queued at 80 columns, a provider's 30-character id
        // keeps the queue count, and the label that no longer fits whole
        // gives way.
        let id = "toolu_01abcdefghijklmnopqrstuv";
        assert_eq!(id.chars().count(), 30);
        update(
            &mut app,
            Msg::Runtime(crate::runtime::RuntimeUpdate::Approvals(vec![
                approval(id),
                approval("call_2"),
            ])),
        );
        let (rows, _) = draw(&mut app, 80, 24);
        let head = &rows[row_index(&rows, "Approval required")];
        assert!(
            head.contains(&format!("Approval required: bash  {id}/2  1/2")),
            "{head}"
        );
        assert!(!head.contains(copy::APPROVAL_FROM_CODEMODE), "{head}");
        // Where the label fits, it follows the count.
        let (rows, _) = draw(&mut app, 120, 24);
        let head = &rows[row_index(&rows, "Approval required")];
        assert!(
            head.contains(&format!("{id}/2  1/2  from codemode toolu_01abc…")),
            "{head}"
        );
    }

    #[test]
    fn a_long_detail_scrolls_on_a_narrow_terminal_and_a_short_one_does_not_arm() {
        let content: Vec<String> = (1..=40).map(|n| format!("  line {n}")).collect();
        let arguments = format!("path: notes.txt\ncontent:\n{}", content.join("\n"));
        let mut app = asking(&"needs it ".repeat(20), &arguments);
        let (rows, _) = draw(&mut app, 40, 30);
        let screen = rows.join("\n");
        // Eleven detail rows, then where they are in the whole.
        assert!(screen.contains("1-11/"), "{screen}");
        assert_eq!(app.approval_drawn(), Some(1));
        // Paging reaches the last line, and no further.
        for _ in 0..20 {
            update(&mut app, Msg::Key(KeyInput::plain(Key::PageDown)));
        }
        let (rows, _) = draw(&mut app, 40, 30);
        let screen = rows.join("\n");
        assert!(screen.contains("line 40"), "{screen}");
        let total = approval_detail(&app, 36).len();
        assert_eq!(app.approval_scroll, total - 11);
        assert_eq!(app.approval_drawn(), None, "still the same showing");
        // Title and keys alone, without a detail row, are not shown whole.
        let mut short = asking("why", "command: x");
        let (rows, _) = draw(&mut short, 80, 2);
        assert!(rows.join("\n").contains("Approval required"));
        assert_eq!(short.approval_drawn(), None);
        // Four rows: the draft's first row, then title, one detail row, keys.
        let (_, _) = draw(&mut short, 80, 3);
        assert_eq!(short.approval_drawn(), None);
        let (_, _) = draw(&mut short, 80, 4);
        assert_eq!(short.approval_drawn(), Some(1));
    }

    // The shells' row sits under the composer box while one runs, and the
    // list and a shell take the body with their keys on the bar.
    #[test]
    fn background_shells_draw_under_the_box_and_in_the_body() {
        use crate::jobs::{BackgroundJob, JobState};
        use crate::runtime::RuntimeUpdate;
        use crate::state::{Msg, update};
        let mut app = State::live(FrameStyle::Round, Tones::None, Vec::new());
        let (rows, _) = draw(&mut app, 60, 12);
        assert!(!rows.iter().any(|row| row.contains("manage")), "{rows:#?}");
        update(
            &mut app,
            Msg::Runtime(RuntimeUpdate::Jobs(vec![BackgroundJob {
                id: "bash-1".into(),
                command: "bun run build".into(),
                state: JobState::Running,
                elapsed: Duration::from_secs(72),
                output: "compiling\nerror: boom\n".into(),
                omitted_bytes: 0,
            }])),
        );
        let (rows, _) = draw(&mut app, 60, 12);
        let last = rows.last().unwrap();
        assert!(
            last.contains("● bash-1  bun run build  1m 12s"),
            "{rows:#?}"
        );
        assert!(last.trim_end().ends_with("↓ manage"), "{last}");
        assert!(
            rows[rows.len() - 2].contains('╰'),
            "the row is under the box: {rows:#?}"
        );
        update(&mut app, Msg::Key(KeyInput::plain(Key::Down)));
        let (rows, _) = draw(&mut app, 60, 12);
        assert!(
            rows.iter()
                .any(|row| row.contains("› ● bash-1  bun run build")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("Background shells")),
            "{rows:#?}"
        );
        assert!(!rows.last().unwrap().contains("manage"));
        update(&mut app, Msg::Key(KeyInput::plain(Key::Enter)));
        let (rows, _) = draw(&mut app, 60, 12);
        assert!(
            rows.iter().any(|row| row.contains("$ bun run build")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("error: boom")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Background shells / bash-1")),
            "{rows:#?}"
        );
    }
}
