//! The fullscreen transcript: rows, how each is presented at a width, and a
//! viewport over them that follows new output or holds a reading position.
//!
//! A user's words run beside an accent bar, reasoning (dim, italic) and
//! answers are prose at column 2, and a call is one block: its state mark,
//! the tool name in an aligned column, the argument, a summary right-aligned,
//! and its output hung from a dim gutter. A blank row opens each section: a
//! user turn, a reasoning block, a group of calls, and an answer. Lines wrap,
//! never truncate, and a result's output is previewed, not replayed.
//!
//! The viewport presents only the rows it visits: following output, it
//! measures from the newest row up; reading, from its anchor down. Neither
//! measures the whole history.

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity::Tones;
use crate::copy;
use crate::editor;

/// Column a user's words, reasoning, answers, and call marks start at.
pub const RAIL: usize = 2;
/// Output lines a call's preview draws, first and last, around a count of
/// the rest; the TypeScript `resultLines` default.
pub const RESULT_LINES: usize = 4;
/// Lines of overlap a page keeps, so the reader does not lose their place.
pub const PAGE_OVERLAP: usize = 4;

/// How a call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    Running,
    Done,
    Failed,
}

/// One committed transcript row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// The preview's title and introduction, which open the transcript.
    Welcome,
    /// The user's words that started a turn.
    User(String),
    /// The model's working-out, dim and italic.
    Reasoning(String),
    /// The model's answer.
    Answer(String),
    /// A tool call and its result as one block.
    Call {
        tool: String,
        argument: String,
        state: CallState,
        /// The card's headline on the head, such as `3 matches` or `exit 1`.
        summary: Option<String>,
        output: Vec<String>,
    },
}

/// The fixed sample session the preview opens with; nothing in it ran.
pub fn sample_session() -> Vec<Row> {
    let call =
        |tool: &str, argument: &str, state, summary: Option<&str>, output: &[&str]| Row::Call {
            tool: tool.into(),
            argument: argument.into(),
            state,
            summary: summary.map(str::to_owned),
            output: output.iter().map(|line| (*line).to_owned()).collect(),
        };
    vec![
        Row::Welcome,
        Row::User("Find where the session controller registers commands".into()),
        Row::Reasoning("The registry is the list, so discovery should read it.".into()),
        call(
            "Bash",
            r#"rg -n "commands.register" -g '*.ts'"#,
            CallState::Done,
            None,
            &["packages/app/src/controller.ts:45", "packages/app/src/controller.ts:52"],
        ),
        call("Read", "packages/app/src/controller.ts", CallState::Done, Some("412 lines"), &[]),
        Row::Answer(
            "Two registrations, both through ctx.effect, so each one is disposed with the plugin that made it."
                .into(),
        ),
        Row::User("Run the parser tests".into()),
        call(
            "Bash",
            "bun test tests/parser.test.ts",
            CallState::Failed,
            Some("exit 1"),
            &[
                "bun test v1.3.0",
                "tests/parser.test.ts:",
                "(pass) splits fields",
                "(pass) keeps quoted commas",
                "(pass) trims trailing blanks",
                "(fail) rejects an unterminated quote",
                " 3 pass",
                " 1 fail",
            ],
        ),
        Row::Answer(
            "One test fails: an unterminated quote is accepted. The parser should reject it before splitting."
                .into(),
        ),
    ]
}

/// Whether a blank row opens `row`, given the row before it: each section
/// opens with one, and a call directly after a call joins its group.
fn opens_section(previous: Option<&Row>, row: &Row) -> bool {
    !matches!(
        (previous, row),
        (None, _) | (Some(Row::Call { .. }), Row::Call { .. })
    )
}

/// `text` wrapped into rows of at most `width` cells, as the composer wraps:
/// breaks after spaces, a word longer than a row split between graphemes,
/// and no wrapped row opening with a space.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let layout = editor::layout(text, 0, width + 1);
    layout
        .rows
        .iter()
        .map(|row| {
            editor::display(&text[row.start..row.end], width)
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The marks the transcript draws. The round set matches the composer's
/// rounded frame; the classic set is ASCII, for terminals that get the
/// classic frame, since a mark drawn wider than measured shifts its row.
#[derive(Debug, PartialEq, Eq)]
pub struct Marks {
    /// The bar beside each row of the user's words.
    pub user: &'static str,
    pub done: &'static str,
    pub failed: &'static str,
    pub running: &'static str,
    /// The rule a call's output hangs from.
    pub gutter: &'static str,
    /// Before the count of output lines left out.
    pub more: &'static str,
}

pub const ROUND_MARKS: Marks = Marks {
    user: "▎",
    done: "✓",
    failed: "✗",
    running: "●",
    gutter: "│",
    more: "⋯",
};

pub const CLASSIC_MARKS: Marks = Marks {
    user: "|",
    done: "+",
    failed: "x",
    running: "*",
    gutter: "|",
    more: "...",
};

/// How the transcript is drawn: its colours and its marks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub tones: Tones,
    pub classic: bool,
}

impl Look {
    pub fn marks(self) -> &'static Marks {
        if self.classic {
            &CLASSIC_MARKS
        } else {
            &ROUND_MARKS
        }
    }
}

/// Column a call's tool name starts at, after its state mark.
pub const TOOL: usize = 4;
/// Cells the tool name's column takes, its gap included, so arguments align.
pub const TOOL_WIDTH: usize = 6;
/// Column a call's output starts at, after the gutter at [`RAIL`] + 2.
pub const OUTPUT: usize = 6;

/// `text` wrapped at `width` less `indent`; each row after the first opens
/// with `rest`, the first with `first`.
fn hang(
    text: &str,
    width: usize,
    indent: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    style: Style,
) -> Vec<Line<'static>> {
    let room = width.saturating_sub(indent).max(1);
    wrap(text, room)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let mut spans = if i == 0 { first.clone() } else { rest.clone() };
            spans.push(Span::styled(row, style));
            Line::from(spans)
        })
        .collect()
}

fn pad(cells: usize) -> Span<'static> {
    Span::raw(" ".repeat(cells))
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// A palette colour on a truecolor terminal, its nearest ANSI colour
/// otherwise, and none under `NO_COLOR`.
fn colour(tones: Tones, rgb: (u8, u8, u8), ansi: Color) -> Style {
    match tones {
        Tones::TrueColor => Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        Tones::Ansi => Style::new().fg(ansi),
        Tones::None => Style::new(),
    }
}

fn accent(tones: Tones) -> Style {
    colour(tones, (0x0e, 0xa5, 0xe9), Color::Cyan)
}

/// The lines `rows[index]` draws at `width`, its opening blank included.
pub fn present(rows: &[Row], index: usize, width: usize, look: Look) -> Vec<Line<'static>> {
    let row = &rows[index];
    let marks = look.marks();
    let mut lines = Vec::new();
    if opens_section(index.checked_sub(1).map(|i| &rows[i]), row) {
        lines.push(Line::default());
    }
    let rail = || vec![pad(RAIL)];
    match row {
        Row::Welcome => {
            lines.push(Line::from(vec![
                pad(RAIL),
                Span::styled(copy::TITLE, accent(look.tones).add_modifier(Modifier::BOLD)),
            ]));
            for paragraph in copy::INTRO.iter().filter(|p| !p.is_empty()) {
                lines.extend(hang(paragraph, width, RAIL, rail(), rail(), dim()));
            }
        }
        // A bar runs beside every row of the user's words, so a long prompt
        // reads as one block.
        Row::User(text) => {
            let bar = vec![Span::styled(format!("{} ", marks.user), accent(look.tones))];
            lines.extend(hang(
                text,
                width,
                RAIL,
                bar.clone(),
                bar,
                Style::new().add_modifier(Modifier::BOLD),
            ));
        }
        Row::Reasoning(text) => lines.extend(hang(
            text,
            width,
            RAIL,
            rail(),
            rail(),
            dim().add_modifier(Modifier::ITALIC),
        )),
        Row::Answer(text) => lines.extend(hang(text, width, RAIL, rail(), rail(), Style::new())),
        Row::Call {
            tool,
            argument,
            state,
            summary,
            output,
        } => lines.extend(present_call(
            tool,
            argument,
            *state,
            summary.as_deref(),
            output,
            width,
            look,
        )),
    }
    lines
}

/// A call as one block: `✓ Bash  argument` with its summary right-aligned,
/// then its output hung from a gutter.
fn present_call(
    tool: &str,
    argument: &str,
    state: CallState,
    summary: Option<&str>,
    output: &[String],
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let marks = look.marks();
    let red = colour(look.tones, (0xef, 0x44, 0x44), Color::Red);
    let (mark, mark_style) = match state {
        CallState::Running => (
            marks.running,
            colour(look.tones, (0xf9, 0x73, 0x16), Color::LightRed),
        ),
        CallState::Done => (
            marks.done,
            colour(look.tones, (0x22, 0xc5, 0x5e), Color::Green),
        ),
        CallState::Failed => (marks.failed, red),
    };
    let failed = state == CallState::Failed;
    let previewed = preview(output);
    // Without a summary of its own, a call with output says how much.
    let counted = output.iter().filter(|line| !line.trim().is_empty()).count();
    let summary = summary.map(str::to_owned).or_else(|| match counted {
        0 => None,
        1 => Some(format!("1 {}", copy::LINE)),
        n => Some(format!("{n} {}", copy::LINES)),
    });
    let name = format!("{tool:<width$}", width = TOOL_WIDTH.max(tool.width() + 1));
    let indent = TOOL + name.width();
    let mut lines = hang(
        argument,
        width,
        indent,
        vec![
            pad(RAIL),
            Span::styled(mark, mark_style),
            pad(TOOL - RAIL - 1),
            Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
        ],
        vec![pad(indent)],
        Style::new(),
    );
    if let Some(summary) = summary {
        let style = if failed { red } else { dim() };
        let last = lines.last_mut().expect("a head has a row");
        let used = last.width();
        if used + 2 + summary.width() <= width {
            last.spans.push(pad(width - used - summary.width()));
            last.spans.push(Span::styled(summary, style));
        } else if summary.width() + RAIL <= width {
            lines.push(Line::from(vec![
                pad(width - summary.width()),
                Span::styled(summary, style),
            ]));
        }
    }
    let gutter_style = if failed { red } else { dim() };
    let gutter = || {
        vec![
            pad(RAIL + 2),
            Span::styled(format!("{} ", marks.gutter), gutter_style),
        ]
    };
    for item in previewed {
        match item {
            Preview::Line(text) => lines.extend(hang(
                &text,
                width,
                OUTPUT,
                gutter(),
                gutter(),
                output_tone(look.tones),
            )),
            Preview::More(count) => lines.push(Line::from(
                gutter()
                    .into_iter()
                    .chain([Span::styled(
                        format!("{} {count} {}", marks.more, copy::MORE_LINES),
                        dim(),
                    )])
                    .collect::<Vec<_>>(),
            )),
        }
    }
    lines
}

fn output_tone(tones: Tones) -> Style {
    colour(tones, (0xb4, 0xb8, 0xbf), Color::Gray)
}

/// One line of a result's preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    Line(String),
    /// The count of lines between the first and the last.
    More(usize),
}

/// The output a call draws: every line when there are at most
/// [`RESULT_LINES`] (or one more, since a count standing for one line is
/// that line), else the first half and the last half around a count.
/// Blank lines at either end are left out.
pub fn preview(output: &[String]) -> Vec<Preview> {
    let blank = |line: &&String| line.trim().is_empty();
    let start = output
        .iter()
        .position(|line| !blank(&line))
        .unwrap_or(output.len());
    let end = output
        .iter()
        .rposition(|line| !blank(&line))
        .map_or(start, |i| i + 1);
    let lines = &output[start..end];
    if lines.len() <= RESULT_LINES + 1 {
        return lines.iter().cloned().map(Preview::Line).collect();
    }
    let head = RESULT_LINES / 2;
    let tail = RESULT_LINES - head;
    let mut out: Vec<Preview> = lines[..head].iter().cloned().map(Preview::Line).collect();
    out.push(Preview::More(lines.len() - RESULT_LINES));
    out.extend(
        lines[lines.len() - tail..]
            .iter()
            .cloned()
            .map(Preview::Line),
    );
    out
}

/// The first line the viewport shows: a row and a line within it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchor {
    pub row: usize,
    pub line: usize,
}

/// The transcript's rows and the viewport over them. `anchor` is `None`
/// while following new output, which holds the newest line at the bottom.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    pub rows: Vec<Row>,
    pub anchor: Option<Anchor>,
    /// The viewport's size on the last frame; paging needs both.
    pub width: usize,
    pub height: usize,
    /// How the last frame drew, so measuring matches drawing.
    pub look: Look,
}

impl Transcript {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }

    fn lines(&self, index: usize) -> Vec<Line<'static>> {
        present(&self.rows, index, self.width, self.look)
    }

    fn count(&self, index: usize) -> usize {
        self.lines(index).len()
    }

    /// The top of the viewport while following: walks up from the newest
    /// row until the viewport is full.
    fn bottom_anchor(&self) -> Anchor {
        let mut need = self.height;
        for index in (0..self.rows.len()).rev() {
            let count = self.count(index);
            if count >= need {
                return Anchor {
                    row: index,
                    line: count - need,
                };
            }
            need -= count;
        }
        Anchor::default()
    }

    /// Where the viewport's top is, whether following or reading.
    pub fn top(&self) -> Anchor {
        self.anchor.unwrap_or_else(|| self.bottom_anchor())
    }

    /// Whether the viewport is following new output.
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }

    /// Lines from `anchor` down, at most `limit`.
    fn lines_from(&self, anchor: Anchor, limit: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        let mut skip = anchor.line;
        for index in anchor.row..self.rows.len() {
            for line in self.lines(index).into_iter().skip(skip) {
                if out.len() == limit {
                    return out;
                }
                out.push(line);
            }
            skip = 0;
        }
        out
    }

    /// The lines the viewport shows, `height` of them at most.
    pub fn visible(&self) -> Vec<Line<'static>> {
        self.lines_from(self.top(), self.height)
    }

    /// Whether lines lie below the viewport.
    pub fn more_below(&self) -> bool {
        !self.following() && self.lines_from(self.top(), self.height + 1).len() > self.height
    }

    /// Whether lines lie above the viewport.
    pub fn more_above(&self) -> bool {
        self.top() != Anchor::default()
    }

    /// Moves the viewport up `by` lines; reaching the start holds there.
    pub fn scroll_up(&mut self, by: usize) {
        let Anchor { mut row, mut line } = self.top();
        let mut by = by;
        while by > 0 {
            if line >= by {
                line -= by;
                break;
            }
            by -= line;
            if row == 0 {
                line = 0;
                break;
            }
            row -= 1;
            line = self.count(row);
        }
        self.anchor = Some(Anchor { row, line });
    }

    /// Moves the viewport down `by` lines; reaching the newest line follows
    /// output again.
    pub fn scroll_down(&mut self, by: usize) {
        let Some(Anchor { mut row, mut line }) = self.anchor else {
            return;
        };
        let bottom = self.bottom_anchor();
        line += by;
        while row < self.rows.len() {
            let count = self.count(row);
            if line < count {
                break;
            }
            line -= count;
            row += 1;
        }
        let past_bottom = row >= self.rows.len() || (row, line) >= (bottom.row, bottom.line);
        self.anchor = (!past_bottom).then_some(Anchor { row, line });
    }

    /// One page: the viewport less [`PAGE_OVERLAP`] lines, and at least half
    /// of it.
    pub fn page(&self) -> usize {
        self.height
            .saturating_sub(PAGE_OVERLAP)
            .max(self.height / 2)
            .max(1)
    }

    pub fn to_start(&mut self) {
        self.anchor = Some(Anchor::default());
        if self.bottom_anchor() == Anchor::default() {
            self.anchor = None;
        }
    }

    pub fn follow(&mut self) {
        self.anchor = None;
    }

    /// The line of a user row that holds its first row, past its opening blank.
    fn prompt_line(&self, index: usize) -> usize {
        usize::from(opens_section(
            index.checked_sub(1).map(|i| &self.rows[i]),
            &self.rows[index],
        ))
    }

    /// Brings the previous user prompt to the top; past the first, the start.
    pub fn previous_prompt(&mut self) {
        let top = self.top();
        let target = (0..self.rows.len()).rev().find(|&index| {
            matches!(self.rows[index], Row::User(_))
                && (index, self.prompt_line(index)) < (top.row, top.line)
        });
        match target {
            Some(index) => {
                self.anchor = Some(Anchor {
                    row: index,
                    line: self.prompt_line(index),
                });
                self.settle();
            }
            None => self.to_start(),
        }
    }

    /// Brings the next user prompt to the top; past the last, follows output.
    pub fn next_prompt(&mut self) {
        let top = self.top();
        let target = (0..self.rows.len()).find(|&index| {
            matches!(self.rows[index], Row::User(_))
                && (index, self.prompt_line(index)) > (top.row, top.line)
        });
        match target {
            Some(index) => {
                self.anchor = Some(Anchor {
                    row: index,
                    line: self.prompt_line(index),
                });
                self.settle();
            }
            None => self.follow(),
        }
    }

    /// Follows output when the anchor is at or past the bottom, so the
    /// viewport never shows blank rows under the newest line.
    fn settle(&mut self) {
        if let Some(anchor) = self.anchor {
            let bottom = self.bottom_anchor();
            if (anchor.row, anchor.line) >= (bottom.row, bottom.line) {
                self.anchor = None;
            }
        }
    }

    /// Records the viewport's size for this frame and keeps the anchor in
    /// range after a width change renumbers a row's lines.
    pub fn resize(&mut self, width: usize, height: usize, look: Look) {
        self.width = width;
        self.height = height;
        self.look = look;
        if let Some(anchor) = self.anchor {
            let count = self.count(anchor.row.min(self.rows.len().saturating_sub(1)));
            self.anchor = Some(Anchor {
                row: anchor.row,
                line: anchor.line.min(count.saturating_sub(1)),
            });
            self.settle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Look = Look {
        tones: Tones::None,
        classic: false,
    };

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    }

    fn session(width: usize, height: usize) -> Transcript {
        let mut t = Transcript::new(sample_session());
        t.resize(width, height, PLAIN);
        t
    }

    #[test]
    fn rows_read_as_blocks_with_aligned_calls() {
        let rows = sample_session();
        let all: Vec<String> = (0..rows.len())
            .flat_map(|i| text(&present(&rows, i, 80, PLAIN)))
            .collect();
        let at = |needle: &str| all.iter().position(|l| l.contains(needle)).unwrap();
        let user = at("Find where");
        assert_eq!(
            all[user],
            "▎ Find where the session controller registers commands"
        );
        assert_eq!(all[user - 1], "", "a blank opens the turn");
        assert_eq!(all[user + 1], "");
        assert_eq!(
            all[user + 2],
            "  The registry is the list, so discovery should read it."
        );
        assert_eq!(all[user + 3], "");
        // The tool name has its own column; the summary is right-aligned.
        let head = &all[user + 4];
        assert!(
            head.starts_with(r#"  ✓ Bash  rg -n "commands.register" -g '*.ts'"#),
            "{head}"
        );
        assert!(head.ends_with("2 lines") && head.width() == 80, "{head}");
        assert_eq!(all[user + 5], "    │ packages/app/src/controller.ts:45");
        assert_eq!(all[user + 6], "    │ packages/app/src/controller.ts:52");
        // A call after a call joins its group without a blank.
        assert!(all[user + 7].starts_with("  ✓ Read  packages/app/src/controller.ts"));
        assert!(all[user + 7].ends_with("412 lines"));
        assert_eq!(all[user + 8], "");
        assert!(all[user + 9].starts_with("  Two registrations"));
    }

    #[test]
    fn a_long_result_is_previewed_first_and_last_around_a_count() {
        let rows = sample_session();
        let index = rows
            .iter()
            .position(|r| {
                matches!(
                    r,
                    Row::Call {
                        state: CallState::Failed,
                        ..
                    }
                )
            })
            .unwrap();
        let lines = text(&present(&rows, index, 60, PLAIN));
        assert_eq!(lines[0], "");
        assert_eq!(
            lines[1],
            format!(
                "  ✗ Bash  bun test tests/parser.test.ts{}exit 1",
                " ".repeat(15)
            )
        );
        assert_eq!(
            lines[2..],
            [
                "    │ bun test v1.3.0",
                "    │ tests/parser.test.ts:",
                "    │ ⋯ 4 more lines",
                "    │  3 pass",
                "    │  1 fail",
            ]
        );
        let lines = |n: usize| (0..n).map(|i| format!("l{i}")).collect::<Vec<_>>();
        // A count standing for one line is that line; blank ends are dropped.
        assert_eq!(preview(&lines(5)).len(), 5);
        let mut padded = vec![String::new()];
        padded.extend(lines(3));
        padded.push("  ".into());
        assert_eq!(
            preview(&padded),
            lines(3).into_iter().map(Preview::Line).collect::<Vec<_>>()
        );
        assert_eq!(preview(&lines(9))[2], Preview::More(5));
    }

    #[test]
    fn classic_frames_draw_ascii_marks() {
        let rows = sample_session();
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        let all: Vec<String> = (0..rows.len())
            .flat_map(|i| text(&present(&rows, i, 80, classic)))
            .collect();
        assert!(
            all.iter().all(|l| l.is_ascii() || l.contains('·')),
            "{all:#?}"
        );
        assert!(all.iter().any(|l| l.starts_with("| Run the parser tests")));
        assert!(all.iter().any(|l| l.starts_with("  x Bash  bun test")));
        assert!(all.iter().any(|l| l == "    | ... 4 more lines"));
    }

    #[test]
    fn prose_and_arguments_wrap_under_their_own_columns() {
        let rows = vec![Row::Answer("alpha beta gamma delta epsilon".into())];
        assert_eq!(
            text(&present(&rows, 0, 14, PLAIN)),
            ["  alpha beta", "  gamma delta", "  epsilon"]
        );
        let user = vec![Row::User("alpha beta gamma".into())];
        assert_eq!(
            text(&present(&user, 0, 10, PLAIN)),
            ["▎ alpha", "▎ beta", "▎ gamma"]
        );
        let call = vec![Row::Call {
            tool: "Bash".into(),
            argument: "one two three four".into(),
            state: CallState::Running,
            summary: None,
            output: vec!["0123456789abcdef".into()],
        }];
        assert_eq!(
            text(&present(&call, 0, 18, PLAIN)),
            [
                "  ● Bash  one two",
                "          three",
                "          four",
                "            1 line",
                "    │ 0123456789ab",
                "    │ cdef",
            ]
        );
    }

    #[test]
    fn following_shows_the_newest_lines_and_paging_holds_a_reading_position() {
        let mut t = session(80, 6);
        assert!(t.following());
        let last = text(&t.visible());
        assert_eq!(last.len(), 6);
        assert_eq!(last[5], "  before splitting.");
        assert!(t.more_above() && !t.more_below());

        t.scroll_up(t.page());
        assert!(!t.following());
        let read = text(&t.visible());
        assert_ne!(read, last);
        assert!(t.more_below());
        // Output arriving below does not move what is being read.
        t.rows.push(Row::Answer("A later answer.".into()));
        assert_eq!(text(&t.visible()), read);

        // Paging back down reaches the bottom and follows again.
        for _ in 0..10 {
            t.scroll_down(t.page());
        }
        assert!(t.following());
        assert_eq!(text(&t.visible())[5], "  A later answer.");
    }

    #[test]
    fn a_page_keeps_four_lines_and_at_least_half_the_viewport() {
        assert_eq!(session(80, 20).page(), 16);
        assert_eq!(session(80, 6).page(), 3);
        assert_eq!(session(80, 1).page(), 1);
    }

    #[test]
    fn prompts_jump_to_the_top_and_the_ends_reach_start_and_output() {
        let mut t = session(80, 6);
        t.previous_prompt();
        assert_eq!(text(&t.visible())[0], "▎ Run the parser tests");
        t.previous_prompt();
        assert_eq!(
            text(&t.visible())[0],
            "▎ Find where the session controller registers commands"
        );
        t.previous_prompt();
        assert_eq!(t.top(), Anchor::default());
        assert_eq!(text(&t.visible())[0], "  Bake · Rust preview");
        t.next_prompt();
        assert!(text(&t.visible())[0].starts_with("▎ Find where"));
        t.next_prompt();
        t.next_prompt();
        assert!(t.following());
        t.to_start();
        assert_eq!(t.top(), Anchor::default());
        t.follow();
        assert!(t.following());
    }

    #[test]
    fn a_short_history_never_leaves_the_bottom() {
        let mut t = Transcript::new(vec![Row::Answer("only".into())]);
        t.resize(80, 10, PLAIN);
        t.scroll_up(5);
        t.scroll_down(1);
        assert!(t.following());
        t.to_start();
        assert!(t.following());
        assert_eq!(text(&t.visible()), ["  only"]);
    }

    #[test]
    fn a_width_change_keeps_the_anchor_inside_its_row() {
        let mut t = session(80, 6);
        t.scroll_up(3);
        let before = t.top();
        t.resize(20, 6, PLAIN);
        let after = t.top();
        assert_eq!(after.row, before.row);
        assert!(after.line < t.count(after.row));
    }
}
