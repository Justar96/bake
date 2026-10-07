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

use std::time::Duration;

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
    /// A code-mode program (`run_code`): what it is for, its source, the
    /// calls it made, and what it returned.
    Script {
        description: String,
        source: Vec<String>,
        state: CallState,
        calls: Vec<Nested>,
        /// What the program returned, or its error; absent while it runs.
        result: Option<String>,
    },
}

/// One call a script made, drawn on the script's tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nested {
    pub tool: String,
    pub argument: String,
    pub state: CallState,
    /// What it returned in brief, such as `2 lines`, or its error when it failed.
    pub note: Option<String>,
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
        Row::User("Find TODO comments in the source files".into()),
        sample_script(),
        Row::Answer(
            "Two files have TODOs: src/m3.ts and src/m9.ts. src/m5.ts could not be read: permission denied."
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

/// A sample code-mode program that reads every source file, one of which
/// it may not read.
fn sample_script() -> Row {
    let read = |index: usize| Nested {
        tool: "Read".into(),
        argument: format!("src/m{index}.ts"),
        state: if index == 5 {
            CallState::Failed
        } else {
            CallState::Done
        },
        note: Some(if index == 5 {
            "Permission denied".into()
        } else {
            "2 lines".into()
        }),
    };
    let mut calls = vec![Nested {
        tool: "Glob".into(),
        argument: "src/**/*.ts".into(),
        state: CallState::Done,
        note: Some("12 files".into()),
    }];
    calls.extend((0..12).map(read));
    Row::Script {
        description: "Find TODOs".into(),
        source: [
            "const found = [];",
            "for (const path of await tools.glob({ pattern: \"src/**/*.ts\" })) {",
            "  const text = await tools.read({ path });",
            "  if (text.includes(\"TODO\")) found.push(path);",
            "}",
            "return found;",
        ]
        .map(str::to_owned)
        .to_vec(),
        state: CallState::Done,
        calls,
        result: Some(r#"["src/m3.ts", "src/m9.ts"]"#.into()),
    }
}

/// Whether a blank row opens `row`, given the row before it: each section
/// opens with one, and a call directly after a call joins its group.
fn opens_section(previous: Option<&Row>, row: &Row) -> bool {
    let call = |row: &Row| matches!(row, Row::Call { .. } | Row::Script { .. });
    match previous {
        None => false,
        Some(previous) => !(call(previous) && call(row)),
    }
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
    /// A script's tree: a call that has one after it, the last call, and the
    /// stem that continues past a call's own lines.
    pub branch: &'static str,
    pub corner: &'static str,
    pub stem: &'static str,
    /// Before what a script returned.
    pub result: &'static str,
}

pub const ROUND_MARKS: Marks = Marks {
    user: "▎",
    done: "✓",
    failed: "✗",
    running: "●",
    gutter: "│",
    more: "⋯",
    branch: "├",
    corner: "└",
    stem: "│",
    result: "→",
};

pub const CLASSIC_MARKS: Marks = Marks {
    user: "|",
    done: "+",
    failed: "x",
    running: "*",
    gutter: "|",
    more: "...",
    branch: "|",
    corner: "`",
    stem: "|",
    result: ">",
};

/// How the transcript is drawn: its colours, its marks, and whether a
/// running call's blinking mark is shown at this moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    pub tones: Tones,
    pub classic: bool,
    /// The blink's phase, from [`lit`]: shown, or a blank of the mark's width.
    pub lit: bool,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            tones: Tones::default(),
            classic: false,
            lit: true,
        }
    }
}

/// How long a running call's mark is shown, and then as long hidden; the
/// TypeScript `PULSE_MS`, four of its 150 ms beats.
pub const PULSE: Duration = Duration::from_millis(600);

/// Whether a running call's mark is shown at `now`. Every running mark blinks
/// in phase, so several cost no more redraws than one.
pub fn lit(now: Duration) -> bool {
    (now.as_millis() / PULSE.as_millis()).is_multiple_of(2)
}

/// Time from `now` until the blink next changes phase.
pub fn next_pulse(now: Duration) -> Duration {
    let step = PULSE.as_millis();
    Duration::from_millis(u64::try_from(step - now.as_millis() % step).unwrap_or(1))
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
        Row::Script {
            description,
            source,
            state,
            calls,
            result,
        } => lines.extend(present_script(
            description,
            source,
            *state,
            calls,
            result.as_deref(),
            width,
            look,
        )),
    }
    lines
}

/// A call state's mark and its colour: a white dot that blinks while the
/// call runs, a green check when it is done, and a red cross when it failed.
fn mark_of(state: CallState, look: Look) -> (&'static str, Style) {
    let marks = look.marks();
    match state {
        // A clean on and off, not a dim half-state, so it reads the same on
        // every theme and under `NO_COLOR`; hidden, a blank keeps the row still.
        CallState::Running if !look.lit => (" ", Style::new()),
        CallState::Running => (
            marks.running,
            colour(look.tones, (0xff, 0xff, 0xff), Color::White).add_modifier(Modifier::BOLD),
        ),
        CallState::Done => (
            marks.done,
            colour(look.tones, (0x22, 0xc5, 0x5e), Color::Green),
        ),
        CallState::Failed => (marks.failed, red(look)),
    }
}

fn red(look: Look) -> Style {
    colour(look.tones, (0xef, 0x44, 0x44), Color::Red)
}

/// A call's head: `lead`, its state mark, the tool name in its column, and
/// the argument wrapped under itself. Wrapped rows open with `rest`, which
/// takes as many cells as `lead`.
fn head(
    lead: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    state: CallState,
    tool: &str,
    argument: &str,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let (mark, mark_style) = mark_of(state, look);
    let lead_cells: usize = lead.iter().map(Span::width).sum();
    let name = format!("{tool:<width$}", width = TOOL_WIDTH.max(tool.width() + 2));
    let indent = lead_cells + 2 + name.width();
    let mut first = lead;
    first.extend([
        Span::styled(mark, mark_style),
        pad(1),
        Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
    ]);
    let mut rest = rest;
    rest.push(pad(indent - lead_cells));
    hang(argument, width, indent, first, rest, Style::new())
}

/// Puts `summary` at the right edge of the last line, or right-aligned on a
/// line of its own, opening with `lead`, when it does not fit beside it.
fn ride(
    lines: &mut Vec<Line<'static>>,
    summary: Vec<Span<'static>>,
    lead: Vec<Span<'static>>,
    width: usize,
) {
    let cells: usize = summary.iter().map(Span::width).sum();
    if cells == 0 {
        return;
    }
    let last = lines.last_mut().expect("a head has a row");
    let used = last.width();
    if used + 2 + cells <= width {
        last.spans.push(pad(width - used - cells));
        last.spans.extend(summary);
    } else {
        let lead_cells: usize = lead.iter().map(Span::width).sum();
        if lead_cells + 2 + cells > width {
            return;
        }
        let mut spans = lead;
        spans.push(pad(width - lead_cells - cells));
        spans.extend(summary);
        lines.push(Line::from(spans));
    }
}

/// A count of lines: `1 line`, `2 lines`; nothing for none.
fn line_count(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some(format!("1 {}", copy::LINE)),
        n => Some(format!("{n} {}", copy::LINES)),
    }
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
    let failed = state == CallState::Failed;
    let quiet = if failed { red(look) } else { dim() };
    let mut lines = head(
        vec![pad(RAIL)],
        vec![pad(RAIL)],
        state,
        tool,
        argument,
        width,
        look,
    );
    // Without a summary of its own, a call with output says how much.
    let counted = output.iter().filter(|line| !line.trim().is_empty()).count();
    if let Some(summary) = summary.map(str::to_owned).or_else(|| line_count(counted)) {
        ride(
            &mut lines,
            vec![Span::styled(summary, quiet)],
            Vec::new(),
            width,
        );
    }
    let gutter = || {
        vec![
            pad(RAIL + 2),
            Span::styled(format!("{} ", marks.gutter), quiet),
        ]
    };
    for item in preview(output) {
        match item {
            Preview::Line(text) => lines.extend(hang(
                &text,
                width,
                OUTPUT,
                gutter(),
                gutter(),
                output_tone(look.tones),
            )),
            Preview::More(count) => {
                let mut spans = gutter();
                spans.push(Span::styled(
                    format!("{} {count} {}", marks.more, copy::MORE_LINES),
                    dim(),
                ));
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}

/// Calls a script's tree keeps at each end, around the counts its middle
/// folds into; the TypeScript `NESTED_ENDS`.
pub const NESTED_ENDS: usize = 2;
/// Failed or unfinished calls the tree keeps from its folded middle, where
/// they are the news; the TypeScript `NESTED_FAILURES`.
pub const NESTED_FAILURES: usize = 3;

/// One entry of a script's tree: a call, or a count of calls folded away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch<'a> {
    Call(&'a Nested),
    Folded(usize),
}

/// The tree a script's calls print as: the first and last [`NESTED_ENDS`],
/// up to [`NESTED_FAILURES`] calls from the middle that did not succeed, and
/// a count for each run of the rest. A count that would stand for one call
/// is that call instead.
pub fn tree(calls: &[Nested]) -> Vec<Branch<'_>> {
    let ends = calls.len().saturating_sub(NESTED_ENDS);
    let mut news = 0;
    let kept: Vec<bool> = calls
        .iter()
        .enumerate()
        .map(|(i, call)| {
            i < NESTED_ENDS
                || i >= ends
                || (call.state != CallState::Done && {
                    news += 1;
                    news <= NESTED_FAILURES
                })
        })
        .collect();
    let mut out = Vec::new();
    for (i, call) in calls.iter().enumerate() {
        let lone = (i == 0 || kept[i - 1]) && kept.get(i + 1).is_none_or(|k| *k);
        if kept[i] || lone {
            out.push(Branch::Call(call));
        } else if let Some(Branch::Folded(count)) = out.last_mut() {
            *count += 1;
        } else {
            out.push(Branch::Folded(1));
        }
    }
    out
}

/// A code-mode program as one block: `✓ Script  description` with its call
/// count, its source numbered and previewed, the calls it made on a tree,
/// and what it returned after `→`.
fn present_script(
    description: &str,
    source: &[String],
    state: CallState,
    calls: &[Nested],
    result: Option<&str>,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let marks = look.marks();
    let mut lines = head(
        vec![pad(RAIL)],
        vec![pad(RAIL)],
        state,
        copy::SCRIPT,
        description,
        width,
        look,
    );
    // Failures are counted once the script ends; a running one shows each
    // failed call red on its own row.
    if !calls.is_empty() {
        let noun = if calls.len() == 1 {
            copy::CALL
        } else {
            copy::CALLS
        };
        let mut tally = vec![Span::styled(format!("{} {noun}", calls.len()), dim())];
        let failed = calls
            .iter()
            .filter(|c| c.state == CallState::Failed)
            .count();
        if state != CallState::Running && failed > 0 {
            tally.push(Span::styled(
                format!(" · {failed} {}", copy::FAILED),
                red(look),
            ));
        }
        ride(&mut lines, tally, Vec::new(), width);
    }
    lines.extend(numbered(source, width, marks));
    let entries = tree(calls);
    for (i, entry) in entries.iter().enumerate() {
        let last = i + 1 == entries.len();
        let glyph = if last { marks.corner } else { marks.branch };
        let lead = vec![pad(RAIL + 2), Span::styled(format!("{glyph} "), dim())];
        let rest = vec![
            pad(RAIL + 2),
            Span::styled(format!("{} ", if last { " " } else { marks.stem }), dim()),
        ];
        match entry {
            Branch::Folded(count) => {
                let mut spans = lead;
                spans.push(Span::styled(
                    format!("{} {count} {}", marks.more, copy::MORE_CALLS),
                    dim(),
                ));
                lines.push(Line::from(spans));
            }
            Branch::Call(call) => {
                let mut rows = head(
                    lead,
                    rest.clone(),
                    call.state,
                    &call.tool,
                    &call.argument,
                    width,
                    look,
                );
                if let Some(note) = &call.note {
                    let style = if call.state == CallState::Failed {
                        red(look)
                    } else {
                        dim()
                    };
                    ride(
                        &mut rows,
                        vec![Span::styled(note.clone(), style)],
                        rest,
                        width,
                    );
                }
                lines.extend(rows);
            }
        }
    }
    if let Some(result) = result {
        let style = if state == CallState::Failed {
            red(look)
        } else {
            Style::new()
        };
        let returned: Vec<String> = result.lines().map(str::to_owned).collect();
        let mark = || {
            vec![
                pad(RAIL + 2),
                Span::styled(format!("{} ", marks.result), dim()),
            ]
        };
        let mut first = true;
        for item in preview(&returned) {
            let lead = if first { mark() } else { vec![pad(OUTPUT)] };
            first = false;
            match item {
                Preview::Line(text) => {
                    lines.extend(hang(&text, width, OUTPUT, lead, vec![pad(OUTPUT)], style))
                }
                Preview::More(count) => {
                    let mut spans = lead;
                    spans.push(Span::styled(
                        format!("{} {count} {}", marks.more, copy::MORE_LINES),
                        dim(),
                    ));
                    lines.push(Line::from(spans));
                }
            }
        }
    }
    lines
}

/// A script's source as the block previews it: each line after its number,
/// dim, and long sources as their first and last lines around a count.
fn numbered(source: &[String], width: usize, marks: &Marks) -> Vec<Line<'static>> {
    let digits = source.len().to_string().len();
    let indent = RAIL + 2 + digits + 2;
    let mut lines = Vec::new();
    let mut number = 0;
    for item in preview_all(source) {
        match item {
            Preview::Line(text) => {
                number += 1;
                let lead = vec![
                    pad(RAIL + 2),
                    Span::styled(format!("{number:>digits$}  "), dim()),
                ];
                lines.extend(hang(
                    &text,
                    width,
                    indent,
                    lead,
                    vec![pad(indent)],
                    Style::new(),
                ));
            }
            Preview::More(count) => {
                number += count;
                lines.push(Line::from(vec![
                    pad(RAIL + 2),
                    Span::styled(
                        format!("{} {count} {}", marks.more, copy::MORE_LINES),
                        dim(),
                    ),
                ]));
            }
        }
    }
    lines
}

/// [`preview`] without dropping blank lines at the ends, so source keeps
/// its line numbers.
fn preview_all(lines: &[String]) -> Vec<Preview> {
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

    /// Whether any call, or any call a script made, is still running, so its
    /// mark blinks.
    pub fn running(&self) -> bool {
        self.rows.iter().any(|row| match row {
            Row::Call { state, .. } => *state == CallState::Running,
            Row::Script { state, calls, .. } => {
                *state == CallState::Running || calls.iter().any(|c| c.state == CallState::Running)
            }
            _ => false,
        })
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
        lit: true,
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
            "▎ Find TODO comments in the source files"
        );
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

    fn script_lines(width: usize, look: Look) -> Vec<String> {
        let rows = sample_session();
        let index = rows
            .iter()
            .position(|r| matches!(r, Row::Script { .. }))
            .unwrap();
        text(&present(&rows, index, width, look))
    }

    #[test]
    fn a_script_shows_its_source_calls_and_result_as_one_block() {
        let lines = script_lines(80, PLAIN);
        let right = |left: &str, right: &str| {
            format!(
                "{left}{}{right}",
                " ".repeat(80 - left.width() - right.width())
            )
        };
        assert_eq!(
            lines,
            [
                String::new(),
                right("  ✓ Script  Find TODOs", "13 calls · 1 failed"),
                "    1  const found = [];".into(),
                r#"    2  for (const path of await tools.glob({ pattern: "src/**/*.ts" })) {"#
                    .into(),
                "    ⋯ 2 more lines".into(),
                "    5  }".into(),
                "    6  return found;".into(),
                right("    ├ ✓ Glob  src/**/*.ts", "12 files"),
                right("    ├ ✓ Read  src/m0.ts", "2 lines"),
                "    ├ ⋯ 4 more calls".into(),
                right("    ├ ✗ Read  src/m5.ts", "Permission denied"),
                "    ├ ⋯ 4 more calls".into(),
                right("    ├ ✓ Read  src/m10.ts", "2 lines"),
                right("    └ ✓ Read  src/m11.ts", "2 lines"),
                r#"    → ["src/m3.ts", "src/m9.ts"]"#.into(),
            ]
        );
    }

    #[test]
    fn a_narrow_script_keeps_its_tree_unbroken() {
        let lines = script_lines(40, PLAIN);
        let failed = lines.iter().position(|l| l.contains("src/m5.ts")).unwrap();
        // The error moves under its call, and the stem runs beside it.
        assert_eq!(
            lines[failed + 1],
            format!("    │{}Permission denied", " ".repeat(18))
        );
        assert!(lines.iter().all(|l| l.width() <= 40));
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        let ascii = script_lines(80, classic);
        assert!(
            ascii.iter().all(|l| l.is_ascii() || l.contains('·')),
            "{ascii:#?}"
        );
        assert!(
            ascii
                .iter()
                .any(|l| l.starts_with("    ` + Read  src/m11.ts"))
        );
        assert!(ascii.iter().any(|l| l.starts_with(r#"    > ["src/m3.ts""#)));
    }

    #[test]
    fn the_tree_keeps_its_ends_and_failures_and_folds_the_rest() {
        let call = |state| Nested {
            tool: "Read".into(),
            argument: String::new(),
            state,
            note: None,
        };
        let shape = |states: &[CallState]| -> Vec<String> {
            let calls: Vec<Nested> = states.iter().map(|s| call(*s)).collect();
            tree(&calls)
                .iter()
                .map(|b| match b {
                    Branch::Call(c) => format!("{:?}", c.state),
                    Branch::Folded(n) => format!("+{n}"),
                })
                .collect()
        };
        use CallState::{Done as D, Failed as F, Running as R};
        assert_eq!(shape(&[D, D, D, D]), ["Done"; 4]);
        // A count standing for one call is that call.
        assert_eq!(shape(&[D, D, D, D, D]), ["Done"; 5]);
        assert_eq!(
            shape(&[D, D, D, D, D, D]),
            ["Done", "Done", "+2", "Done", "Done"]
        );
        // Up to three failed or unfinished calls stay where they were; the
        // fourth folds with its neighbours.
        assert_eq!(
            shape(&[D, D, D, F, D, F, D, F, D, F, D, D, R]),
            [
                "Done", "Done", "Done", "Failed", "Done", "Failed", "Done", "Failed", "+3", "Done",
                "Running"
            ]
            .map(str::to_owned)
        );
        assert!(tree(&[]).is_empty());
    }

    #[test]
    fn a_running_mark_blinks_white_in_place() {
        let rows = vec![Row::Call {
            tool: "Bash".into(),
            argument: "bun run build".into(),
            state: CallState::Running,
            summary: None,
            output: Vec::new(),
        }];
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let on = present(&rows, 0, 40, look);
        let off = present(&rows, 0, 40, Look { lit: false, ..look });
        assert_eq!(text(&on), ["  ● Bash  bun run build"]);
        assert_eq!(text(&off), ["    Bash  bun run build"]);
        assert_eq!(on[0].width(), off[0].width());
        let mark = on[0].spans.iter().find(|s| s.content == "●").unwrap();
        assert_eq!(mark.style.fg, Some(Color::Rgb(0xff, 0xff, 0xff)));
        // Shown for one pulse, hidden for the next; the loop wakes at each change.
        assert!(lit(Duration::ZERO) && lit(Duration::from_millis(599)));
        assert!(!lit(PULSE) && lit(PULSE * 2));
        assert_eq!(
            next_pulse(Duration::from_millis(250)),
            Duration::from_millis(350)
        );
        let mut t = Transcript::new(rows);
        assert!(t.running());
        t.rows = sample_session();
        assert!(!t.running());
    }
}
