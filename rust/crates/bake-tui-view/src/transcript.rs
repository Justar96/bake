//! The fullscreen transcript: rows, how each is presented at a width, and a
//! viewport over them that follows new output or holds a reading position.
//!
//! A user's words run beside an accent bar, reasoning (dim, italic) and
//! answers are prose at column 2, and a call is one block: its state mark,
//! the tool name in an aligned column, the argument, its status after it,
//! and its output hung from a dim gutter, in a light box on a truecolor
//! terminal. A blank row opens every row but the first. Lines wrap, never
//! truncate, and a result's output is previewed, not replayed.
//!
//! The viewport presents only the rows it visits: following output, it
//! measures from the newest row up; reading, from its anchor down. Neither
//! measures the whole history.

use std::ops::Range;
use std::time::Duration;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity::Tones;
use crate::copy;
use crate::diff::{self, Change, DiffLine, DiffRow, SplitRow};
use crate::editor;
use crate::shell_output::{self, Family, Styled};
use crate::syntax::{self, Carry, Lang};

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
    /// calls it made through its `tools` bindings, and what it handed back
    /// to the model: the lines it logged and the value it returned.
    Script {
        description: String,
        source: Vec<String>,
        state: CallState,
        /// A status for the head after the call count, such as `interrupted`.
        summary: Option<String>,
        calls: Vec<Nested>,
        /// What the program wrote with `console.log`, in order.
        logs: Vec<String>,
        /// What the program returned, or its error; absent while it runs.
        result: Option<String>,
    },
}

/// One call a script made, drawn on its call site under the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nested {
    /// The tool's registry name, which is also its binding: `read` is
    /// called as `tools.read`.
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
            &[
                "packages/app/src/controller.ts:45:  commands.register(open);",
                "packages/app/src/controller.ts:52:  commands.register(close);",
            ],
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
        Row::Reasoning("The splitter keeps an unterminated quote as text; it should throw instead.".into()),
        call(
            "Edit",
            "src/parser.ts",
            CallState::Done,
            None,
            &[
                "@@ -41,4 +41,4 @@",
                "   const fields = split(line);",
                "-  if (quote) fields.push(rest);",
                "+  if (quote) throw new SyntaxError(\"unterminated quote\");",
                "   return fields;",
            ],
        ),
        call(
            "Bash",
            "bun test tests/parser.test.ts",
            CallState::Done,
            Some("exit 0"),
            &["(pass) rejects an unterminated quote", " 4 pass", " 0 fail"],
        ),
        Row::Answer(
            "One test failed because an unterminated quote was accepted. The parser now rejects it before splitting."
                .into(),
        ),
    ]
}

/// A sample code-mode program that reads every source file, one of which
/// it may not read: it catches that call's error and logs it, so the model
/// learns of it without the program failing.
fn sample_script() -> Row {
    let read = |index: usize| Nested {
        tool: "read".into(),
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
        tool: "glob".into(),
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
            "  try {",
            "    if ((await tools.read({ path })).includes(\"TODO\")) found.push(path);",
            "  } catch (error) {",
            "    console.log(`skipped ${path}: ${error.message}`);",
            "  }",
            "}",
            "return found;",
        ]
        .map(str::to_owned)
        .to_vec(),
        state: CallState::Done,
        summary: None,
        calls,
        logs: vec!["skipped src/m5.ts: Permission denied".into()],
        result: Some(r#"["src/m3.ts", "src/m9.ts"]"#.into()),
    }
}

/// Rows that open `row`, given the row before it: none for the first row, a
/// dotted rule and a blank before a user's turn, and a blank before any
/// other row, so each call's box stands apart from the next.
fn opening(previous: Option<&Row>, row: &Row, width: usize, look: Look) -> Vec<Line<'static>> {
    match (previous, row) {
        (None, _) => Vec::new(),
        (Some(_), Row::User(_)) => vec![rule(width, look), Line::default()],
        (Some(_), _) => vec![Line::default()],
    }
}

/// A dim dotted rule across the transcript, inside its rail on both sides,
/// as a printed catalogue rules off a section.
pub fn rule(width: usize, look: Look) -> Line<'static> {
    let cells = width.saturating_sub(2 * RAIL);
    Line::from(vec![
        pad(RAIL),
        Span::styled(look.marks().rule.repeat(cells), dim()),
    ])
}

/// The tint of a [`tag`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagTone {
    Yellow,
    Pink,
    Blue,
    Green,
    Red,
}

/// A short code set off as a tag, as a catalogue prints a stock number: the
/// text with a cell of padding on each side, light on a dark tint of its
/// tone. Sixteen colours draw the text in the tone without a tint, and
/// `NO_COLOR` draws it plain; the padding keeps its width in every case.
pub fn tag(text: &str, tone: TagTone, tones: Tones) -> Span<'static> {
    let ((fg, bg), ansi) = match tone {
        TagTone::Yellow => (((0xfd, 0xe6, 0x8a), (0x3a, 0x34, 0x16)), Color::Yellow),
        TagTone::Pink => (((0xf9, 0xa8, 0xd4), (0x3b, 0x1f, 0x33)), Color::Magenta),
        TagTone::Blue => (((0x93, 0xc5, 0xfd), (0x1c, 0x2b, 0x44)), Color::Blue),
        TagTone::Green => (((0x86, 0xef, 0xac), (0x17, 0x33, 0x22)), Color::Green),
        TagTone::Red => (((0xfc, 0xa5, 0xa5), (0x3f, 0x1d, 0x22)), Color::Red),
    };
    let style = match tones {
        Tones::TrueColor => Style::new()
            .fg(Color::Rgb(fg.0, fg.1, fg.2))
            .bg(Color::Rgb(bg.0, bg.1, bg.2)),
        Tones::Ansi => Style::new().fg(ansi),
        Tones::None => Style::new(),
    };
    Span::styled(format!(" {text} "), style)
}

/// A call's status as drawn: an exit code or an interruption as a tag, green
/// for success and red or yellow otherwise, and any other status as text.
fn status_spans(summary: &str, quiet: Style, look: Look) -> Vec<Span<'static>> {
    let tone = match summary.strip_prefix("exit ") {
        Some("0") => Some(TagTone::Green),
        Some(code) if code.parse::<i32>().is_ok() => Some(TagTone::Red),
        _ if summary == copy::INTERRUPTED_NOTE => Some(TagTone::Yellow),
        _ => None,
    };
    match tone {
        Some(tone) => vec![tag(summary, tone, look.tones)],
        None => vec![Span::styled(summary.to_owned(), quiet)],
    }
}

/// The palette the transcript draws with, as a strip of swatches: the hues,
/// then the greys. A catalogue prints its inks the same way.
const SWATCHES: &[(u8, u8, u8, Color)] = &[
    (0xef, 0x44, 0x44, Color::Red),
    (0xf9, 0x73, 0x16, Color::LightRed),
    (0xfb, 0xbf, 0x24, Color::Yellow),
    (0x22, 0xc5, 0x5e, Color::Green),
    (0x2d, 0xd4, 0xbf, Color::Cyan),
    (0x7d, 0xd3, 0xfc, Color::LightCyan),
    (0x60, 0xa5, 0xfa, Color::Blue),
    (0xa7, 0x8b, 0xfa, Color::LightMagenta),
    (0xf4, 0x72, 0xb6, Color::Magenta),
];
const GREYS: &[(u8, u8, u8, Color)] = &[
    (0xe5, 0xe7, 0xeb, Color::White),
    (0xd1, 0xd5, 0xdb, Color::Gray),
    (0x9c, 0xa3, 0xaf, Color::Gray),
    (0x6b, 0x72, 0x80, Color::DarkGray),
    (0x4b, 0x55, 0x63, Color::DarkGray),
    (0x37, 0x41, 0x51, Color::DarkGray),
];
/// Cells each swatch takes.
const SWATCH: usize = 2;

/// The swatch strip, or nothing where it cannot be drawn faithfully: under
/// `NO_COLOR`, and with the classic frame, whose terminals may draw a block
/// glyph at a width other than the one measured.
fn swatches(look: Look) -> Vec<Span<'static>> {
    if look.tones == Tones::None || look.classic {
        return Vec::new();
    }
    let swatch = |&(r, g, b, ansi): &(u8, u8, u8, Color)| {
        let colour = if look.tones == Tones::TrueColor {
            Color::Rgb(r, g, b)
        } else {
            ansi
        };
        Span::styled("█".repeat(SWATCH), Style::new().fg(colour))
    };
    let mut spans: Vec<Span<'static>> = SWATCHES.iter().map(swatch).collect();
    spans.push(pad(1));
    spans.extend(GREYS.iter().map(swatch));
    spans
}

/// Whether calls are drawn in boxes: on a truecolor terminal.
fn boxes(look: Look) -> bool {
    look.tones == Tones::TrueColor
}

/// The gutter a call's output hangs from: the rule without a box, and a
/// blank inside one, where the box already holds the call together.
fn gutter(look: Look) -> &'static str {
    if boxes(look) {
        " "
    } else {
        look.marks().gutter
    }
}

/// The background of a call's box: a soft grey a few steps above a dark
/// terminal's own, and a faint red for a call that failed. Only a truecolor
/// terminal draws it; sixteen colours have no step that subtle, and
/// `NO_COLOR` draws none.
pub fn box_colour(state: CallState, tones: Tones) -> Option<Color> {
    (tones == Tones::TrueColor).then_some(match state {
        CallState::Failed => Color::Rgb(0x2e, 0x22, 0x25),
        CallState::Running | CallState::Done => Color::Rgb(0x25, 0x28, 0x2f),
    })
}

/// Makes `lines` a card: a padding row above and below, and every row
/// filled with `bg` across the full `width`.
fn card(lines: &mut Vec<Line<'static>>, width: usize, bg: Color) {
    lines.insert(0, Line::default());
    lines.push(Line::default());
    boxed(lines, width, bg);
}

/// Fills `lines` with `bg` across the full `width`, under every span that
/// sets no background of its own, so the call reads as one box.
fn boxed(lines: &mut [Line<'static>], width: usize, bg: Color) {
    for line in lines {
        for span in &mut line.spans {
            if span.style.bg.is_none() {
                span.style = span.style.bg(bg);
            }
        }
        let used = line.width();
        if used < width {
            line.spans
                .push(Span::styled(" ".repeat(width - used), Style::new().bg(bg)));
        }
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
    /// Before a call site, under the program line that made its calls.
    pub site: &'static str,
    /// In a program's gutter, beside the line whose calls are in flight.
    pub active: &'static str,
    /// A call site's meter: a call done, still running, failed, and
    /// stopped by an interruption.
    pub meter_done: &'static str,
    pub meter_running: &'static str,
    pub meter_failed: &'static str,
    pub meter_stopped: &'static str,
    /// The dotted rule that opens a user's turn.
    pub rule: &'static str,
}

pub const ROUND_MARKS: Marks = Marks {
    user: ">",
    done: "✓",
    failed: "✗",
    running: "●",
    gutter: "│",
    more: "⋯",
    site: "╰",
    active: "▸",
    meter_done: "━",
    meter_running: "╌",
    meter_failed: "✗",
    meter_stopped: "╳",
    rule: "┄",
};

pub const CLASSIC_MARKS: Marks = Marks {
    user: ">",
    done: "+",
    failed: "x",
    running: "*",
    gutter: "|",
    more: "...",
    site: "`",
    active: ">",
    meter_done: "=",
    meter_running: "-",
    meter_failed: "x",
    meter_stopped: "/",
    rule: "-",
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

/// Cells the tool name's column takes, its colon and gap included, so the
/// arguments of the common four-letter tools align: `Bash: `, `Read: `.
pub const TOOL_WIDTH: usize = 6;
/// Column everything inside a call starts at: its output, a diff, a
/// script's source, and the counts of what is folded away. Column [`RAIL`]
/// holds the call's structure: its state mark, the gutter, a script's mark
/// of the line in flight, and the labels of what it handed back.
pub const BODY: usize = 4;
/// Cells a call's text keeps clear of the box's right edge.
pub const BOX_PAD: usize = 2;

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

/// `text` with each tab turned into the spaces to its next stop, so the
/// lexer's byte ranges and the drawn cells agree.
fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + editor::TAB_WIDTH);
    let mut column = 0;
    for c in text.chars() {
        if c == '\t' {
            let spaces = editor::TAB_WIDTH - column % editor::TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            out.push(c);
            column += c.to_string().width();
        }
    }
    out
}

/// The styled runs of one line of code in `lang`, or none for plain text.
fn code_runs(
    line: &str,
    lang: Option<Lang>,
    carry: &mut Carry,
    tones: Tones,
) -> Vec<(Range<usize>, Style)> {
    let Some(lang) = lang else {
        return Vec::new();
    };
    syntax::tokens(line, lang, carry)
        .into_iter()
        .map(|(range, token)| (range, syntax::style(token, tones)))
        .collect()
}

/// [`hang`] for text with styled runs: wraps `text` the same way, and each
/// row keeps the style its bytes have, so a token split by a wrap keeps its
/// colour on both rows. Bytes no run covers take `base`; a run's style is
/// laid over `base`. `text` must hold no tabs.
fn hang_runs(
    text: &str,
    runs: &[(Range<usize>, Style)],
    base: Style,
    width: usize,
    indent: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let room = width.saturating_sub(indent).max(1);
    let layout = editor::layout(text, 0, room + 1);
    layout
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut spans = if i == 0 { first.clone() } else { rest.clone() };
            let end = row.start + text[row.start..row.end].trim_end().len();
            let mut at = row.start;
            for (range, style) in runs {
                let (from, to) = (range.start.max(at), range.end.min(end));
                if from >= to {
                    continue;
                }
                if at < from {
                    spans.push(Span::styled(text[at..from].to_owned(), base));
                }
                spans.push(Span::styled(text[from..to].to_owned(), base.patch(*style)));
                at = to;
            }
            if at < end {
                spans.push(Span::styled(text[at..end].to_owned(), base));
            }
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
    lines.extend(opening(
        index.checked_sub(1).map(|i| &rows[i]),
        row,
        width,
        look,
    ));
    let rail = || vec![pad(RAIL)];
    // Inside a box, a call's text keeps clear of the box's right edge.
    let inner = if boxes(look) {
        width.saturating_sub(BOX_PAD).max(1)
    } else {
        width
    };
    match row {
        Row::Welcome => {
            let mut title = vec![
                pad(RAIL),
                Span::styled(copy::TITLE, accent(look.tones).add_modifier(Modifier::BOLD)),
            ];
            // The palette beside the title, when it fits whole.
            let strip = swatches(look);
            let cells: usize = strip.iter().map(Span::width).sum();
            if cells > 0 && RAIL + copy::TITLE.width() + 2 + cells <= width {
                title.push(pad(2));
                title.extend(strip);
            }
            lines.push(Line::from(title));
            for paragraph in copy::INTRO.iter().filter(|p| !p.is_empty()) {
                lines.extend(hang(paragraph, width, RAIL, rail(), rail(), dim()));
            }
        }
        // A prompt mark opens the user's words, as they were typed; the
        // rows a long prompt wraps to hang under the text, not the mark.
        Row::User(text) => {
            let mark = vec![Span::styled(format!("{} ", marks.user), accent(look.tones))];
            lines.extend(hang(
                text,
                width,
                RAIL,
                mark,
                rail(),
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
        } => {
            let mut block = present_call(
                tool,
                argument,
                *state,
                summary.as_deref(),
                output,
                inner,
                look,
            );
            if let Some(bg) = box_colour(*state, look.tones) {
                card(&mut block, width, bg);
            }
            lines.extend(block);
        }
        Row::Script {
            description,
            source,
            state,
            summary,
            calls,
            logs,
            result,
        } => {
            let mut block = present_script(
                description,
                source,
                *state,
                summary.as_deref(),
                calls,
                logs,
                result.as_deref(),
                inner,
                look,
            );
            if let Some(bg) = box_colour(*state, look.tones) {
                card(&mut block, width, bg);
            }
            lines.extend(block);
        }
    }
    lines
}

/// What kind of work a tool does. It picks whether its argument reads as a
/// path and how its output is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A shell command.
    Shell,
    /// Reading a file.
    Read,
    /// Changing a file; its output is a diff.
    Edit,
    /// Finding files or text.
    Search,
    /// Fetching or searching the web.
    Web,
    /// Delegating to a subagent.
    Agent,
    /// A code-mode program.
    Script,
    Other,
}

/// The kind of a tool, by its name as the session log or the terminal
/// labels it.
pub fn kind(tool: &str) -> Kind {
    match tool.to_ascii_lowercase().as_str() {
        "bash" | "sh" | "zsh" | "shell" | "pwsh" | "powershell" => Kind::Shell,
        "read" | "read_file" | "view" => Kind::Read,
        "edit" | "write" | "write_file" | "multiedit" | "apply_patch" => Kind::Edit,
        "grep" | "glob" | "find" | "search" | "ls" => Kind::Search,
        "fetch" | "web_fetch" | "websearch" | "web_search" => Kind::Web,
        "agent" | "task" | "spawn" => Kind::Agent,
        "codemode" | "script" | "run_code" => Kind::Script,
        _ => Kind::Other,
    }
}

impl Kind {
    /// Whether the argument is a file path, drawn with its directory dim.
    fn takes_path(self) -> bool {
        matches!(self, Self::Read | Self::Edit)
    }
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

/// A code-mode script's mark: braces, `{}`, for the program it is, in
/// its state's colour and blinking while it runs like any call's mark.
/// Under `NO_COLOR` or with the classic frame colour cannot tell the
/// states apart, so it takes a call's mark instead.
fn script_mark(state: CallState, look: Look) -> (&'static str, Style) {
    let (mark, style) = mark_of(state, look);
    if look.tones == Tones::None || look.classic {
        return (mark, style);
    }
    match state {
        CallState::Running if !look.lit => ("  ", style),
        _ => (SCRIPT_MARK, style),
    }
}

/// The code-mode script's mark, two cells wide.
const SCRIPT_MARK: &str = "{}";

fn red(look: Look) -> Style {
    colour(look.tones, (0xef, 0x44, 0x44), Color::Red)
}

/// A call's head: `lead`, its state mark, the tool name in its column, and
/// the argument wrapped under itself. Wrapped rows open with `rest`, which
/// takes as many cells as `lead`. Returns the rows and the lead of a row
/// that continues under the argument, for [`ride`].
fn head(
    lead: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    state: CallState,
    tool: &str,
    argument: &str,
    width: usize,
    look: Look,
) -> (Vec<Line<'static>>, Vec<Span<'static>>) {
    let label = format!("{tool}:");
    let column = TOOL_WIDTH.max(label.width() + 1);
    let name = vec![Span::styled(
        label,
        Style::new().add_modifier(Modifier::BOLD),
    )];
    labelled(
        lead,
        rest,
        state,
        name,
        column,
        kind(tool),
        argument,
        width,
        look,
    )
}

/// [`head`] with its label drawn by the caller: `name` padded to `column`
/// cells, then the argument, which reads as `kind` decides.
#[allow(clippy::too_many_arguments)]
fn labelled(
    lead: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    state: CallState,
    name: Vec<Span<'static>>,
    column: usize,
    kind: Kind,
    argument: &str,
    width: usize,
    look: Look,
) -> (Vec<Line<'static>>, Vec<Span<'static>>) {
    let (mark, mark_style) = if kind == Kind::Script {
        script_mark(state, look)
    } else {
        mark_of(state, look)
    };
    let lead_cells: usize = lead.iter().map(Span::width).sum();
    let name_cells: usize = name.iter().map(Span::width).sum();
    let column = column.max(name_cells + 1);
    let indent = lead_cells + mark.width() + 1 + column;
    let mut first = lead;
    first.extend([Span::styled(mark, mark_style), pad(1)]);
    first.extend(name);
    first.push(pad(column - name_cells));
    let mut rest = rest;
    rest.push(pad(indent - lead_cells));
    // A path that fits reads by its file name: the directory is dim.
    let fits = argument.width() + indent <= width && !argument.contains(char::is_whitespace);
    if kind.takes_path() && fits {
        let split = argument.rfind('/').map_or(0, |i| i + 1);
        first.push(Span::styled(argument[..split].to_owned(), dim()));
        first.push(Span::raw(argument[split..].to_owned()));
        return (vec![Line::from(first)], rest);
    }
    // A shell command reads as code: its program, flags, and strings.
    let argument = expand_tabs(argument);
    let lang = (kind == Kind::Shell).then_some(Lang::Shell);
    let runs = code_runs(&argument, lang, &mut Carry::default(), look.tones);
    (
        hang_runs(
            &argument,
            &runs,
            Style::new(),
            width,
            indent,
            first,
            rest.clone(),
        ),
        rest,
    )
}

/// Puts a call's status, such as `2 lines`, two cells after the last row's
/// text, so it reads with the call at any width. When it does not fit there,
/// it takes rows of its own opening with `lead`, under the argument, and a
/// one-run status wraps there rather than being cut.
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
    if last.width() + 2 + cells <= width {
        last.spans.push(pad(2));
        last.spans.extend(summary);
        return;
    }
    let lead_cells: usize = lead.iter().map(Span::width).sum();
    if lead_cells + cells <= width {
        let mut spans = lead;
        spans.extend(summary);
        lines.push(Line::from(spans));
    } else if let [only] = summary.as_slice() {
        lines.extend(hang(
            &only.content,
            width,
            lead_cells,
            lead.clone(),
            lead,
            only.style,
        ));
    }
}

/// Cells a call's text area needs before an edit's diff goes side by side,
/// so each side keeps about 46 cells.
pub const SPLIT_MIN: usize = 100;
/// The divider between a split diff's sides.
const DIVIDER: &str = " │ ";
/// Diff rows an edit draws before the rest folds into a count.
pub const DIFF_ROWS: usize = 16;

/// The tints of a changed diff line in a call's box: the line, and the
/// stronger one under the part of it that changed.
fn diff_tints(change: Change) -> Option<(Color, Color)> {
    match change {
        Change::Removed => Some((Color::Rgb(0x3d, 0x25, 0x29), Color::Rgb(0x6b, 0x2f, 0x37))),
        Change::Added => Some((Color::Rgb(0x21, 0x3a, 0x2c), Color::Rgb(0x2a, 0x5e, 0x3f))),
        Change::Context => None,
    }
}

/// A change's colour: red removed, green added.
fn change_colour(change: Change, look: Look) -> Style {
    match change {
        Change::Removed => red(look),
        Change::Added => colour(look.tones, (0x22, 0xc5, 0x5e), Color::Green),
        Change::Context => output_tone(look.tones),
    }
}

/// The mark beside a changed line: a coloured bar where colour tells removed
/// from added, and `-` or `+` where it cannot, under `NO_COLOR` or with the
/// classic frame.
fn change_mark(change: Change, look: Look) -> &'static str {
    let bars = look.tones != Tones::None && !look.classic;
    match (change, bars) {
        (Change::Context, _) => " ",
        (_, true) => "▎",
        (Change::Removed, false) => "-",
        (Change::Added, false) => "+",
    }
}

/// `runs` with `emphasis` laid over them: the bytes split at every edge so
/// runs never overlap, and each byte in `emphasis` given `strong` as well as
/// its own style.
fn overlay(
    runs: Vec<(Range<usize>, Style)>,
    emphasis: Option<Range<usize>>,
    strong: Style,
    len: usize,
) -> Vec<(Range<usize>, Style)> {
    let Some(emphasis) = emphasis.filter(|e| !e.is_empty()) else {
        return runs;
    };
    let mut edges: Vec<usize> = runs
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .chain([0, len, emphasis.start, emphasis.end])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges
        .windows(2)
        .map(|w| w[0]..w[1])
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            let own = runs
                .iter()
                .find(|(r, _)| r.start <= seg.start && seg.end <= r.end)
                .map_or(Style::new(), |(_, style)| *style);
            let inside = emphasis.start <= seg.start && seg.end <= emphasis.end;
            (seg, if inside { own.patch(strong) } else { own })
        })
        .collect()
}

/// One side of a diff row, exactly `cells` wide on every row: the line's
/// number right-aligned in `digits` cells, its change mark, and its code
/// wrapped under itself. In a box, a changed line is tinted across the side
/// and the part that changed takes the stronger tint; without one, a changed
/// line takes its colour whole and the changed part is bold. An empty side
/// is a blank row.
fn diff_side(
    line: Option<&DiffLine>,
    cells: usize,
    digits: usize,
    lang: Option<Lang>,
    carry: &mut Carry,
    look: Look,
) -> Vec<Line<'static>> {
    let Some(line) = line else {
        return vec![Line::from(pad(cells))];
    };
    let boxed = boxes(look);
    let tints = if boxed { diff_tints(line.change) } else { None };
    let number_style = match line.change {
        Change::Context => dim(),
        change => change_colour(change, look),
    };
    let number = line.number().map_or(String::new(), |n| n.to_string());
    let mark = Span::styled(
        change_mark(line.change, look),
        change_colour(line.change, look),
    );
    let first = vec![
        Span::styled(format!("{number:>digits$} "), number_style),
        mark.clone(),
        pad(1),
    ];
    let rest = vec![pad(digits + 1), mark, pad(1)];
    let indent = digits + 3;
    let code = expand_tabs(&line.code);
    let (base, runs, strong) = if boxed || line.change == Change::Context {
        let runs = code_runs(&code, lang, carry, look.tones);
        let strong = tints.map_or(Style::new(), |(_, strong)| Style::new().bg(strong));
        (output_tone(look.tones), runs, strong)
    } else {
        // Without a box the line's colour marks it; syntax would hide it.
        code_runs(&code, lang, carry, look.tones);
        (
            change_colour(line.change, look),
            Vec::new(),
            Style::new().add_modifier(Modifier::BOLD),
        )
    };
    let emphasis = line.emphasis.clone().filter(|e| e.end <= code.len());
    let runs = overlay(runs, emphasis, strong, code.len());
    let mut rows = hang_runs(&code, &runs, base, cells, indent, first, rest);
    for row in &mut rows {
        if let Some((tint, _)) = tints {
            for span in &mut row.spans {
                if span.style.bg.is_none() {
                    span.style = span.style.bg(tint);
                }
            }
        }
        let used = row.width();
        let fill = Span::raw(" ".repeat(cells.saturating_sub(used)));
        row.spans.push(match tints {
            Some((tint, _)) => fill.style(Style::new().bg(tint)),
            None => fill,
        });
    }
    rows
}

/// An edit's diff, numbered: unified on a narrow call, side by side once the
/// call's text area reaches [`SPLIT_MIN`] cells. Every row opens with
/// `lead`. Lines a hunk skips read as a count across the diff, and rows past
/// [`DIFF_ROWS`] fold into one.
fn diff_view(
    output: &[String],
    lang: Option<Lang>,
    width: usize,
    look: Look,
    lead: impl Fn() -> Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let marks = look.marks();
    let mut rows = diff::parse(output);
    let folded = rows.len().saturating_sub(DIFF_ROWS);
    rows.truncate(DIFF_ROWS);
    let digits = diff::number_width(&rows);
    let lead_cells: usize = lead().iter().map(Span::width).sum();
    let room = width.saturating_sub(lead_cells);
    let count = |text: String| {
        let mut spans = lead();
        spans.push(pad(digits + 1));
        spans.push(Span::styled(format!("{} {text}", marks.more), dim()));
        Line::from(spans)
    };
    let mut lines = Vec::new();
    if width >= SPLIT_MIN {
        let half = room.saturating_sub(DIVIDER.width());
        let (left_cells, right_cells) = (half / 2, half - half / 2);
        let (mut old, mut new) = (Carry::default(), Carry::default());
        for row in diff::split(&rows) {
            match row {
                SplitRow::Skipped(n) => lines.push(count(format!("{n} {}", copy::UNMODIFIED))),
                SplitRow::Pair(left, right) => {
                    let l = diff_side(left, left_cells, digits, lang, &mut old, look);
                    let r = diff_side(right, right_cells, digits, lang, &mut new, look);
                    for k in 0..l.len().max(r.len()) {
                        let mut spans = lead();
                        // A side shorter than its partner continues its own
                        // tint down the row.
                        let blank = |side: &[Line<'static>], cells: usize| {
                            let bg = side
                                .last()
                                .and_then(|row| row.spans.last())
                                .and_then(|s| s.style.bg);
                            Span::styled(
                                " ".repeat(cells),
                                bg.map_or(Style::new(), |bg| Style::new().bg(bg)),
                            )
                        };
                        match l.get(k) {
                            Some(row) => spans.extend(row.spans.iter().cloned()),
                            None => spans.push(blank(&l, left_cells)),
                        }
                        spans.push(Span::styled(DIVIDER, dim()));
                        match r.get(k) {
                            Some(row) => spans.extend(row.spans.iter().cloned()),
                            None => spans.push(blank(&r, right_cells)),
                        }
                        lines.push(Line::from(spans));
                    }
                }
            }
        }
    } else {
        let mut carry = Carry::default();
        for row in &rows {
            match row {
                DiffRow::Skipped(n) => lines.push(count(format!("{n} {}", copy::UNMODIFIED))),
                DiffRow::Line(line) => {
                    for side in diff_side(Some(line), room, digits, lang, &mut carry, look) {
                        let mut spans = lead();
                        spans.extend(side.spans);
                        lines.push(Line::from(spans));
                    }
                }
            }
        }
    }
    if folded > 0 {
        lines.push(count(format!("{folded} {}", copy::MORE_LINES)));
    }
    lines
}

/// A diff line's colour: green for an added line, red for a removed one, and
/// the output tone for context.
fn diff_style(line: &str, look: Look) -> Style {
    match line.as_bytes().first() {
        Some(b'+') => colour(look.tones, (0x22, 0xc5, 0x5e), Color::Green),
        Some(b'-') => red(look),
        _ => output_tone(look.tones),
    }
}

/// An edit's status: lines added in green and removed in red, `+1 -1`,
/// leaving out a side with none.
fn diff_counts(output: &[String], look: Look) -> Vec<Span<'static>> {
    let count = |sign: u8| {
        output
            .iter()
            .filter(|l| l.as_bytes().first() == Some(&sign))
            .count()
    };
    let (added, removed) = (count(b'+'), count(b'-'));
    let mut spans = Vec::new();
    if added > 0 {
        spans.push(Span::styled(format!("+{added}"), diff_style("+", look)));
    }
    if removed > 0 {
        if !spans.is_empty() {
            spans.push(pad(1));
        }
        spans.push(Span::styled(format!("-{removed}"), red(look)));
    }
    spans
}

/// A count of lines: `1 line`, `2 lines`; nothing for none.
fn line_count(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some(format!("1 {}", copy::LINE)),
        n => Some(format!("{n} {}", copy::LINES)),
    }
}

/// A call as one block: `✓ Bash: argument` with its status after it, then
/// its output hung from a gutter; an edit's output is its diff.
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
    let (mut lines, under) = head(
        vec![pad(RAIL)],
        vec![pad(RAIL)],
        state,
        tool,
        argument,
        width,
        look,
    );
    let edit = kind(tool) == Kind::Edit;
    // Without a summary of its own, an edit says what it added and removed,
    // and any other call with output says how much.
    // Escapes and control characters never reach the screen. A shell
    // command's output keeps its own colour, or takes the colour its shape
    // implies; any other tool's output is plain.
    let shell = kind(tool) == Kind::Shell;
    let family = if shell {
        shell_output::family(argument)
    } else {
        Family::Other
    };
    let cleaned: Vec<Styled> = output
        .iter()
        .map(|line| shell_output::styled(line, family, look.tones))
        .collect();
    let texts: Vec<String> = cleaned.iter().map(|line| line.text.clone()).collect();
    let counted = texts.iter().filter(|line| !line.trim().is_empty()).count();
    let status = match summary {
        Some(summary) => status_spans(summary, quiet, look),
        None if edit => diff_counts(output, look),
        None => line_count(counted)
            .map(|count| vec![Span::styled(count, quiet)])
            .unwrap_or_default(),
    };
    ride(&mut lines, status, under, width);
    let glyph = gutter(look);
    let gutter = || vec![pad(RAIL), Span::styled(format!("{glyph} "), quiet)];
    let lang = if edit {
        syntax::lang_for_path(argument)
    } else {
        None
    };
    if edit {
        lines.extend(diff_view(output, lang, width, look, gutter));
        return lines;
    }
    // The preview keeps lines in order, so each is found from where the
    // last one was.
    let mut at = 0;
    for item in preview(&texts) {
        match item {
            Preview::Line(text) => {
                let found = (at..texts.len()).find(|&i| texts[i] == text).unwrap_or(at);
                at = found + 1;
                let runs = if shell {
                    cleaned.get(found).map_or(&[][..], |line| &line.runs[..])
                } else {
                    &[]
                };
                lines.extend(hang_runs(
                    &text,
                    runs,
                    output_tone(look.tones),
                    width,
                    BODY,
                    gutter(),
                    gutter(),
                ));
            }
            Preview::More(count) => {
                at += count;
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

/// Failed calls a call site lists under itself before the rest are counted.
pub const SITE_FAILURES: usize = 3;
/// Calls a call site's meter draws one cell each; past it, the counts alone
/// say how the calls stand.
pub const METER_MAX: usize = 24;
/// Source lines a program draws whole; a longer one keeps its first and
/// last [`SOURCE_ENDS`] lines and every line a call came from, and counts
/// the rest.
pub const SOURCE_LINES: usize = 12;
pub const SOURCE_ENDS: usize = 2;

/// How a script's program names a tool: `tools.read`, or `tools["my-tool"]`
/// for a name that is not an identifier, as the generated SDK keys it.
pub fn binding(tool: &str) -> String {
    if identifier(tool) {
        format!("tools.{tool}")
    } else {
        format!("tools[{tool:?}]")
    }
}

fn identifier(name: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    name.starts_with(|c: char| word(c) && !c.is_ascii_digit()) && name.chars().all(word)
}

/// Whether `line` of a program calls `tool` through its binding.
fn mentions(line: &str, tool: &str) -> bool {
    if identifier(tool) {
        let needle = format!("tools.{tool}");
        line.match_indices(&needle).any(|(at, _)| {
            !line[at + needle.len()..]
                .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        })
    } else {
        line.contains(&format!("tools[\"{tool}\"]")) || line.contains(&format!("tools['{tool}']"))
    }
}

/// Whether an interruption stopped `call`: it did not fail on its own, so
/// it is not counted or listed as a failure.
fn stopped(call: &Nested) -> bool {
    call.state == CallState::Failed && call.note.as_deref() == Some(copy::INTERRUPTED_NOTE)
}

/// One call site: every call a program made through one binding, drawn
/// under the source line that calls it.
#[derive(Debug)]
pub struct Site<'a> {
    pub tool: &'a str,
    /// The source line that names the binding first, or `None` when no
    /// line does, as when a program picks a tool by a computed name.
    pub line: Option<usize>,
    pub calls: Vec<&'a Nested>,
}

impl Site<'_> {
    fn count(&self, state: CallState) -> usize {
        self.calls
            .iter()
            .filter(|c| c.state == state && !stopped(c))
            .count()
    }

    fn running(&self) -> bool {
        self.count(CallState::Running) > 0
    }
}

/// The calls of a program grouped by the binding they went through, in the
/// order each binding was first called, and placed at the first source line
/// that names it. The session log records which tool each call reached but
/// not which line made it, so a binding named on several lines is drawn at
/// the first.
pub fn sites<'a>(source: &[String], calls: &'a [Nested]) -> Vec<Site<'a>> {
    let mut sites: Vec<Site<'a>> = Vec::new();
    for call in calls {
        match sites.iter_mut().find(|site| site.tool == call.tool) {
            Some(site) => site.calls.push(call),
            None => sites.push(Site {
                tool: &call.tool,
                line: source.iter().position(|line| mentions(line, &call.tool)),
                calls: vec![call],
            }),
        }
    }
    sites
}

/// One entry of a drawn program: a source line, or a count of lines folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    Line(usize),
    Folded(usize),
}

/// The source lines a program draws: all of them up to [`SOURCE_LINES`]
/// (or one more, since a count standing for one line is that line), else
/// the first and last [`SOURCE_ENDS`] and every line in `keep`, with each
/// run of the rest counted.
fn shown(len: usize, keep: &[usize]) -> Vec<Shown> {
    if len <= SOURCE_LINES + 1 {
        return (0..len).map(Shown::Line).collect();
    }
    let kept: Vec<bool> = (0..len)
        .map(|i| i < SOURCE_ENDS || i + SOURCE_ENDS >= len || keep.contains(&i))
        .collect();
    let mut out = Vec::new();
    for i in 0..len {
        let lone = (i == 0 || kept[i - 1]) && kept.get(i + 1).is_none_or(|k| *k);
        if kept[i] || lone {
            out.push(Shown::Line(i));
        } else if let Some(Shown::Folded(count)) = out.last_mut() {
            *count += 1;
        } else {
            out.push(Shown::Folded(1));
        }
    }
    out
}

/// A code-mode program as one block, drawn as the program it is rather
/// than as a list of calls. The head, `✓ Codemode: description` with its
/// call count; the source, numbered and coloured, with each call site hung
/// under the line that makes its calls, `╰ ✓ tools.read ×12` and a meter of
/// how they stand, and a `▶` in the gutter of the line whose calls are in
/// flight; then, after a blank row, all the model reads back, labelled as
/// the program wrote it: `console` for what it logged, `return` for what
/// it returned, or `error` for how it failed.
#[allow(clippy::too_many_arguments)]
fn present_script(
    description: &str,
    source: &[String],
    state: CallState,
    summary: Option<&str>,
    calls: &[Nested],
    logs: &[String],
    result: Option<&str>,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let marks = look.marks();
    let (mut lines, under) = head(
        vec![pad(RAIL)],
        vec![pad(RAIL)],
        state,
        copy::SCRIPT,
        description,
        width,
        look,
    );
    ride(
        &mut lines,
        script_status(state, summary, calls, look),
        under,
        width,
    );

    let sites = sites(source, calls);
    let anchors: Vec<usize> = sites.iter().filter_map(|site| site.line).collect();
    let digits = source.len().max(1).to_string().len();
    // Code starts after the gutter, the number, and two cells; a call site
    // hangs from that column, under the code that made it.
    let code = BODY + digits + 2;
    let source: Vec<String> = source.iter().map(|line| expand_tabs(line)).collect();
    // Lexed in order, every line, so a comment opened on a folded line
    // still colours the lines after it.
    let mut carry = Carry::default();
    let runs: Vec<_> = source
        .iter()
        .map(|line| code_runs(line, Some(Lang::TypeScript), &mut carry, look.tones))
        .collect();
    let glyph = gutter(look);
    let rail = |active: bool| {
        if active {
            vec![
                pad(RAIL),
                Span::styled(
                    format!("{} ", marks.active),
                    accent(look.tones).add_modifier(Modifier::BOLD),
                ),
            ]
        } else {
            vec![pad(RAIL), Span::styled(format!("{glyph} "), dim())]
        }
    };
    for item in shown(source.len(), &anchors) {
        match item {
            Shown::Line(i) => {
                let here: Vec<&Site> = sites.iter().filter(|site| site.line == Some(i)).collect();
                let active = state == CallState::Running && here.iter().any(|site| site.running());
                let mut lead = rail(active);
                lead.push(Span::styled(format!("{:>digits$}  ", i + 1), dim()));
                let mut rest = rail(false);
                rest.push(pad(digits + 2));
                lines.extend(hang_runs(
                    &source[i],
                    &runs[i],
                    Style::new(),
                    width,
                    code,
                    lead,
                    rest,
                ));
                for site in here {
                    lines.extend(present_site(site, rail(false), code - BODY, width, look));
                }
            }
            Shown::Folded(count) => {
                let mut spans = rail(false);
                spans.push(Span::styled(
                    format!("{} {count} {}", marks.more, copy::MORE_LINES),
                    dim(),
                ));
                lines.push(Line::from(spans));
            }
        }
    }
    // Calls through a binding no line names hang after the source.
    for site in sites.iter().filter(|site| site.line.is_none()) {
        lines.extend(present_site(site, rail(false), code - BODY, width, look));
    }
    lines.extend(returned(state, logs, result, width, look));
    lines
}

/// A script head's status, for [`ride`]: how many calls it made, then
/// while it runs how many are in flight, and once it has ended how many
/// failed; then its own status, such as an interruption.
fn script_status(
    state: CallState,
    summary: Option<&str>,
    calls: &[Nested],
    look: Look,
) -> Vec<Span<'static>> {
    let mut status = Vec::new();
    if !calls.is_empty() {
        let noun = if calls.len() == 1 {
            copy::CALL
        } else {
            copy::CALLS
        };
        status.push(Span::styled(format!("{} {noun}", calls.len()), dim()));
        let running = calls
            .iter()
            .filter(|c| c.state == CallState::Running)
            .count();
        if state == CallState::Running && running > 0 {
            status.push(Span::styled(
                format!(" · {running} {}", copy::RUNNING),
                dim(),
            ));
        }
        // The head's own status says what stopped the rest.
        let failed = calls
            .iter()
            .filter(|c| c.state == CallState::Failed && !stopped(c))
            .count();
        if state != CallState::Running && failed > 0 {
            status.push(Span::styled(
                format!(" · {failed} {}", copy::FAILED),
                red(look),
            ));
        }
    }
    if let Some(summary) = summary {
        if !status.is_empty() {
            status.push(pad(2));
        }
        status.extend(status_spans(summary, red(look), look));
    }
    status
}

fn yellow(look: Look) -> Style {
    colour(look.tones, (0xfb, 0xbf, 0x24), Color::Yellow)
}

/// The mark a call site carries: running while any call runs, failed when
/// an interruption cut it short or every call failed, and done otherwise,
/// its failures counted beside it.
fn site_state(site: &Site) -> CallState {
    if site.running() {
        CallState::Running
    } else if site.calls.iter().any(|c| stopped(c))
        || site.calls.iter().all(|c| c.state == CallState::Failed)
    {
        CallState::Failed
    } else {
        CallState::Done
    }
}

/// One call site under its line: `╰ ✓ tools.glob  argument  note` for a
/// single call, and for several, `╰ ● tools.read ×12`, a meter with a cell
/// per call, and how many are done, running, failed, or stopped, with the
/// failures listed under it. `rail` opens every row; `offset` is the cells
/// from the end of `rail` to the code column.
fn present_site(
    site: &Site,
    rail: Vec<Span<'static>>,
    offset: usize,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let marks = look.marks();
    let mut lead = rail.clone();
    lead.push(pad(offset));
    lead.push(Span::styled(format!("{} ", marks.site), dim()));
    let mut rest = rail;
    rest.push(pad(offset + 2));
    let name = binding(site.tool);
    // `tools.` is the same on every site; the tool's name is the news.
    let split = if name.starts_with("tools.") { 6 } else { 5 };
    let mut label = vec![
        Span::styled(name[..split].to_owned(), dim()),
        Span::styled(
            name[split..].to_owned(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ];
    if let [call] = site.calls.as_slice() {
        let column = name.width() + 2;
        let (mut rows, under) = labelled(
            lead,
            rest,
            call.state,
            label,
            column,
            kind(site.tool),
            &call.argument,
            width,
            look,
        );
        if let Some(note) = &call.note {
            let spans = if stopped(call) {
                vec![Span::styled(note.clone(), yellow(look))]
            } else if call.state == CallState::Failed {
                vec![Span::styled(note.clone(), red(look))]
            } else {
                status_spans(note, dim(), look)
            };
            ride(&mut rows, spans, under, width);
        }
        return rows;
    }
    label.push(Span::styled(format!(" ×{}", site.calls.len()), dim()));
    let (mark, mark_style) = mark_of(site_state(site), look);
    let mut first = lead;
    first.extend([Span::styled(mark, mark_style), pad(1)]);
    first.extend(label);
    let mut row = Line::from(first);
    // The meter, a cell per call in the order they were made, when it fits.
    if site.calls.len() <= METER_MAX && row.width() + 2 + site.calls.len() <= width {
        row.spans.push(pad(2));
        row.spans.extend(meter(&site.calls, look));
    }
    let mut rows = vec![row];
    let done = site.count(CallState::Done);
    let running = site.count(CallState::Running);
    let failed = site.count(CallState::Failed);
    let halted = site.calls.iter().filter(|c| stopped(c)).count();
    let mut tally: Vec<Span<'static>> = Vec::new();
    for (count, word, style) in [
        (done, copy::DONE, dim()),
        (running, copy::RUNNING, dim()),
        (failed, copy::FAILED, red(look)),
        (halted, copy::INTERRUPTED_NOTE, yellow(look)),
    ] {
        if count > 0 {
            if !tally.is_empty() {
                tally.push(Span::styled(" · ", dim()));
            }
            tally.push(Span::styled(format!("{count} {word}"), style));
        }
    }
    let mut under = rest.clone();
    under.push(pad(2));
    ride(&mut rows, tally, under.clone(), width);
    // The failures are the news; each is listed with its error.
    let failures: Vec<&&Nested> = site
        .calls
        .iter()
        .filter(|c| c.state == CallState::Failed && !stopped(c))
        .collect();
    for call in failures.iter().take(SITE_FAILURES) {
        let (mark, mark_style) = mark_of(call.state, look);
        let mut first = rest.clone();
        first.extend([Span::styled(mark, mark_style), pad(1)]);
        let indent: usize = first.iter().map(Span::width).sum();
        let mut wrap = rest.clone();
        wrap.push(pad(2));
        let mut found = hang(
            &call.argument,
            width,
            indent,
            first,
            wrap.clone(),
            Style::new(),
        );
        if let Some(note) = &call.note {
            ride(
                &mut found,
                vec![Span::styled(note.clone(), red(look))],
                wrap,
                width,
            );
        }
        rows.extend(found);
    }
    if failures.len() > SITE_FAILURES {
        let mut spans = rest;
        spans.push(Span::styled(
            format!(
                "{} {} {}",
                marks.more,
                failures.len() - SITE_FAILURES,
                copy::MORE_FAILED
            ),
            dim(),
        ));
        rows.push(Line::from(spans));
    }
    rows
}

/// A call site's meter: one cell per call, heavy and green when it is
/// done, red when it failed, yellow when an interruption stopped it, and
/// light while it runs. The glyphs differ too, so it reads without colour.
fn meter(calls: &[&Nested], look: Look) -> Vec<Span<'static>> {
    let marks = look.marks();
    calls
        .iter()
        .map(|call| match call.state {
            CallState::Done => Span::styled(
                marks.meter_done,
                colour(look.tones, (0x22, 0xc5, 0x5e), Color::Green),
            ),
            CallState::Running => Span::styled(marks.meter_running, dim()),
            CallState::Failed if stopped(call) => Span::styled(marks.meter_stopped, yellow(look)),
            CallState::Failed => Span::styled(marks.meter_failed, red(look)),
        })
        .collect()
}

/// All a program hands back to the model, after a blank row and labelled
/// in its own terms: `console` before the lines it logged, then `return`
/// before the value it returned, coloured as code, or `error` before how it
/// failed, in red. Nothing while it runs: the runtime returns both at once.
fn returned(
    state: CallState,
    logs: &[String],
    result: Option<&str>,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let logged = preview(logs);
    if logged.is_empty() && result.is_none() {
        return Vec::new();
    }
    let marks = look.marks();
    let failed = state == CallState::Failed;
    let column = [copy::CONSOLE, copy::RETURN, copy::ERROR]
        .iter()
        .map(|label| label.width())
        .max()
        .unwrap_or(0)
        + 2;
    let indent = RAIL + column;
    let label =
        |text: &str, style: Style| vec![pad(RAIL), Span::styled(format!("{text:<column$}"), style)];
    let mut lines = vec![Line::default()];
    let mut first = true;
    for item in logged {
        let lead = if first {
            label(copy::CONSOLE, dim())
        } else {
            vec![pad(indent)]
        };
        first = false;
        match item {
            Preview::Line(text) => lines.extend(hang(
                &expand_tabs(&text),
                width,
                indent,
                lead,
                vec![pad(indent)],
                output_tone(look.tones),
            )),
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
    let Some(result) = result else {
        return lines;
    };
    let (word, style) = if failed {
        (copy::ERROR, red(look))
    } else {
        (copy::RETURN, Style::new())
    };
    let value: Vec<String> = result.lines().map(str::to_owned).collect();
    // What a script returns is a JavaScript value, so it reads as code; a
    // failed script's error stays red.
    let lang = (!failed).then_some(Lang::TypeScript);
    let mut carry = Carry::default();
    let mut first = true;
    for item in preview(&value) {
        let lead = if first {
            label(word, dim())
        } else {
            vec![pad(indent)]
        };
        first = false;
        match item {
            Preview::Line(text) => {
                let text = expand_tabs(&text);
                let runs = code_runs(&text, lang, &mut carry, look.tones);
                lines.extend(hang_runs(
                    &text,
                    &runs,
                    style,
                    width,
                    indent,
                    lead,
                    vec![pad(indent)],
                ));
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
    /// Each row's height at `measured`, for the scrollbar. Measured once per
    /// width and look, and again for rows appended or changed since.
    heights: Vec<usize>,
    measured: Option<(usize, Tones, bool)>,
    /// Rows there were when output was last followed; rows past it arrived
    /// while reading.
    seen: usize,
}

/// Where the scrollbar's thumb sits on its track: its first row and length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thumb {
    pub start: usize,
    pub len: usize,
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

    /// Brings the row heights up to date: all of them after a width or look
    /// change, and otherwise only rows appended since.
    fn measure(&mut self) {
        let key = (self.width, self.look.tones, self.look.classic);
        if self.measured != Some(key) {
            self.heights.clear();
            self.measured = Some(key);
        }
        self.heights.truncate(self.rows.len());
        for index in self.heights.len()..self.rows.len() {
            let height = self.count(index);
            self.heights.push(height);
        }
    }

    /// Marks row `index` as changed, so its height is measured again.
    pub fn touched(&mut self, index: usize) {
        self.heights.truncate(index);
    }

    /// Whether rows arrived below while reading.
    pub fn new_below(&self) -> bool {
        !self.following() && self.rows.len() > self.seen
    }

    /// Lines in the whole transcript, as last measured.
    pub fn total(&self) -> usize {
        self.heights.iter().sum()
    }

    /// The line the viewport's top is on, counted from the start.
    pub fn offset(&self) -> usize {
        let top = self.top();
        self.heights.iter().take(top.row).sum::<usize>() + top.line
    }

    /// Moves the viewport so its top is line `offset` from the start;
    /// reaching the bottom follows output.
    pub fn jump(&mut self, offset: usize) {
        let mut left = offset;
        let mut anchor = Anchor::default();
        for (row, &height) in self.heights.iter().enumerate() {
            if left < height {
                anchor = Anchor { row, line: left };
                break;
            }
            left -= height;
            anchor = Anchor {
                row: row + 1,
                line: 0,
            };
        }
        if anchor.row >= self.rows.len() {
            self.follow();
            return;
        }
        self.anchor = Some(anchor);
        self.settle();
    }

    /// The scrollbar's thumb on a track of `track` rows, or `None` when the
    /// whole transcript fits. Its length is the visible share, at least one
    /// row; following output, it rests at the bottom.
    pub fn thumb(&self, track: usize) -> Option<Thumb> {
        let total = self.total();
        if track == 0 || total <= self.height {
            return None;
        }
        let len = (track * self.height / total).clamp(1, track);
        let room = track - len;
        let scroll = total - self.height;
        let start = if self.following() {
            room
        } else {
            (self.offset().min(scroll) * room)
                .div_ceil(scroll.max(1))
                .min(room)
        };
        Some(Thumb { start, len })
    }

    /// The offset that puts the thumb's middle on track row `row`, for a
    /// click or a drag on the scrollbar.
    pub fn offset_at(&self, row: usize, track: usize) -> usize {
        let Some(thumb) = self.thumb(track) else {
            return 0;
        };
        let room = track - thumb.len;
        if room == 0 {
            return 0;
        }
        let start = row.saturating_sub(thumb.len / 2).min(room);
        let scroll = self.total() - self.height;
        start * scroll / room
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
        opening(
            index.checked_sub(1).map(|i| &self.rows[i]),
            &self.rows[index],
            self.width,
            self.look,
        )
        .len()
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
        self.measure();
        if self.following() {
            self.seen = self.rows.len();
        }
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
            "> Find where the session controller registers commands"
        );
        assert_eq!(all[user - 1], "", "a blank opens the turn");
        assert_eq!(all[user + 1], "");
        assert_eq!(
            all[user + 2],
            "  The registry is the list, so discovery should read it."
        );
        assert_eq!(all[user + 3], "");
        // The tool name has its own column; the status follows the argument.
        assert_eq!(
            all[user + 4],
            r#"  ✓ Bash: rg -n "commands.register" -g '*.ts'  2 lines"#
        );
        assert!(
            all[user + 5] == "  │ packages/app/src/controller.ts:45:  commands.register(open);"
        );
        assert!(
            all[user + 6] == "  │ packages/app/src/controller.ts:52:  commands.register(close);"
        );
        // Each call stands apart from the next, so its box does too.
        assert_eq!(all[user + 7], "");
        assert_eq!(
            all[user + 8],
            "  ✓ Read: packages/app/src/controller.ts  412 lines"
        );
        assert_eq!(all[user + 9], "");
        assert!(all[user + 10].starts_with("  Two registrations"));
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
        assert_eq!(lines[1], "  ✗ Bash: bun test tests/parser.test.ts   exit 1");
        assert_eq!(
            lines[2..],
            [
                "  │ bun test v1.3.0",
                "  │ tests/parser.test.ts:",
                "  │ ⋯ 4 more lines",
                "  │  3 pass",
                "  │  1 fail",
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
        assert!(all.iter().any(|l| l.starts_with("> Run the parser tests")));
        assert!(all.iter().any(|l| l.starts_with("  x Bash: bun test")));
        assert!(all.iter().any(|l| l == "  | ... 4 more lines"));
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
            ["> alpha", "  beta", "  gamma"]
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
                "  ● Bash: one two",
                "          three",
                "          four",
                "          1 line",
                "  │ 0123456789abcd",
                "  │ ef",
            ]
        );
    }

    #[test]
    fn following_shows_the_newest_lines_and_paging_holds_a_reading_position() {
        let mut t = session(80, 6);
        assert!(t.following());
        let last = text(&t.visible());
        assert_eq!(last.len(), 6);
        assert_eq!(last[5], "  rejects it before splitting.");
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
        assert_eq!(text(&t.visible())[0], "> Run the parser tests");
        t.previous_prompt();
        assert_eq!(
            text(&t.visible())[0],
            "> Find TODO comments in the source files"
        );
        t.previous_prompt();
        assert_eq!(
            text(&t.visible())[0],
            "> Find where the session controller registers commands"
        );
        t.previous_prompt();
        assert_eq!(t.top(), Anchor::default());
        assert_eq!(text(&t.visible())[0], "  Bake · Rust preview");
        t.next_prompt();
        assert!(text(&t.visible())[0].starts_with("> Find where"));
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
    fn only_a_script_head_is_marked_with_braces() {
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let lines = script_lines(80, look);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  {} Codemode: Find TODOs"))
        );
        // Its call sites and every other call keep their state marks.
        assert!(lines.iter().any(|l| l.contains("╰ ✓ tools.glob")));
        let rows = sample_session();
        let all: Vec<String> = (0..rows.len())
            .flat_map(|i| text(&present(&rows, i, 80, look)))
            .collect();
        assert!(all.iter().any(|l| l.starts_with("  ✓ Bash: rg")));
        assert!(all.iter().all(|l| !l.contains("{} Bash")));
        let script = |state, lit| {
            let rows = vec![Row::Script {
                description: "Build".into(),
                state,
                summary: None,
                source: vec!["return 1;".into()],
                calls: Vec::new(),
                logs: Vec::new(),
                result: None,
            }];
            present(&rows, 0, 40, Look { lit, ..look })
        };
        let done = script(CallState::Done, true);
        assert_eq!(fg_of(&done, "{}"), Some(Color::Rgb(0x22, 0xc5, 0x5e)));
        let failed = script(CallState::Failed, true);
        assert_eq!(fg_of(&failed, "{}"), Some(Color::Rgb(0xef, 0x44, 0x44)));
        // Hidden while it blinks, a blank as wide keeps the row still.
        let on = script(CallState::Running, true);
        let off = script(CallState::Running, false);
        assert_eq!(fg_of(&on, "{}"), Some(Color::Rgb(0xff, 0xff, 0xff)));
        assert!(text(&off).iter().any(|l| l == "     Codemode: Build"));
        assert_eq!(text(&on).len(), text(&off).len());
        // Without colour the braces cannot tell the states apart.
        assert!(script_lines(80, PLAIN)[1].starts_with("  ✓ Codemode"));
    }

    #[test]
    fn a_script_reads_as_its_program_with_each_call_site_under_its_line() {
        let lines = script_lines(80, PLAIN);
        // Each status follows its text, two cells after it.
        let then = |left: &str, status: &str| format!("{left}  {status}");
        assert_eq!(
            lines,
            [
                String::new(),
                then("  ✓ Codemode: Find TODOs", "13 calls · 1 failed"),
                "  │ 1  const found = [];".into(),
                r#"  │ 2  for (const path of await tools.glob({ pattern: "src/**/*.ts" })) {"#
                    .into(),
                // A call site hangs from the code column, under its line.
                then("  │    ╰ ✓ tools.glob  src/**/*.ts", "12 files"),
                "  │ 3    try {".into(),
                r#"  │ 4      if ((await tools.read({ path })).includes("TODO")) found.push(path);"#
                    .into(),
                // Twelve calls through one binding are one site: a meter
                // with a cell per call, a tally, and the failure listed.
                then("  │    ╰ ✓ tools.read ×12  ━━━━━✗━━━━━━", "11 done · 1 failed"),
                then("  │      ✗ src/m5.ts", "Permission denied"),
                "  │ 5    } catch (error) {".into(),
                "  │ 6      console.log(`skipped ${path}: ${error.message}`);".into(),
                "  │ 7    }".into(),
                "  │ 8  }".into(),
                "  │ 9  return found;".into(),
                // Apart, all the model reads back, named as the program wrote it.
                String::new(),
                "  console  skipped src/m5.ts: Permission denied".into(),
                r#"  return   ["src/m3.ts", "src/m9.ts"]"#.into(),
            ]
        );
    }

    #[test]
    fn a_tool_is_named_as_its_binding_and_found_where_the_program_calls_it() {
        assert_eq!(binding("read"), "tools.read");
        assert_eq!(binding("web_fetch"), "tools.web_fetch");
        assert_eq!(binding("mcp-sentry"), r#"tools["mcp-sentry"]"#);
        assert_eq!(binding("2fa"), r#"tools["2fa"]"#);
        assert!(mentions("await tools.read({ path })", "read"));
        assert!(!mentions("await tools.readdir({ path })", "read"));
        assert!(mentions(r#"await tools["mcp-sentry"]({})"#, "mcp-sentry"));
        let call = |tool: &str| Nested {
            tool: tool.into(),
            argument: String::new(),
            state: CallState::Done,
            note: None,
        };
        let source = ["const a = await tools.glob({});", "await tools.read({});"]
            .map(str::to_owned)
            .to_vec();
        let calls = [call("read"), call("glob"), call("read"), call("grep")];
        let found: Vec<_> = sites(&source, &calls)
            .iter()
            .map(|site| (site.tool, site.line, site.calls.len()))
            .collect();
        // In the order each binding was first called; a binding no line
        // names, as a computed `tools[name]` would be, has no line.
        assert_eq!(
            found,
            [
                ("read", Some(1), 2),
                ("glob", Some(0), 1),
                ("grep", None, 1)
            ]
        );
    }

    #[test]
    fn a_failed_script_keeps_its_logs_above_its_error() {
        let rows = vec![Row::Script {
            description: "Count lines".into(),
            source: vec![
                "console.log(\"start\");".into(),
                "throw new Error(\"boom\");".into(),
            ],
            state: CallState::Failed,
            summary: None,
            calls: Vec::new(),
            logs: vec!["start".into()],
            result: Some("Error: boom".into()),
        }];
        assert_eq!(
            text(&present(&rows, 0, 60, PLAIN)),
            [
                "  ✗ Codemode: Count lines",
                r#"  │ 1  console.log("start");"#,
                r#"  │ 2  throw new Error("boom");"#,
                "",
                "  console  start",
                "  error    Error: boom",
            ]
        );
    }

    #[test]
    fn a_long_program_folds_around_its_call_sites() {
        let mut source: Vec<String> = (1..=20).map(|n| format!("step{n}();")).collect();
        source[9] = "await tools.read({ path });".into();
        let rows = vec![Row::Script {
            description: "Long".into(),
            source,
            state: CallState::Running,
            summary: None,
            calls: vec![Nested {
                tool: "read".into(),
                argument: "a.ts".into(),
                state: CallState::Running,
                note: None,
            }],
            logs: Vec::new(),
            result: None,
        }];
        let lines = text(&present(&rows, 0, 60, PLAIN));
        assert_eq!(
            lines[1..],
            [
                "  │  1  step1();",
                "  │  2  step2();",
                "  │ ⋯ 7 more lines",
                // The line whose calls are in flight is marked in the gutter.
                "  ▸ 10  await tools.read({ path });",
                "  │     ╰ ● tools.read  a.ts",
                "  │ ⋯ 8 more lines",
                "  │ 19  step19();",
                "  │ 20  step20();",
            ]
        );
    }

    #[test]
    fn a_narrow_script_keeps_its_sites_under_their_lines() {
        let lines = script_lines(38, PLAIN);
        assert!(lines.iter().all(|l| l.width() <= 38), "{lines:#?}");
        let site = lines
            .iter()
            .position(|l| l.contains("tools.read ×12"))
            .unwrap();
        assert!(lines[site].starts_with("  │    ╰ ✓ tools.read ×12"));
        // The tally moves under the label when it does not fit beside it.
        assert_eq!(
            lines[site + 1].trim_start_matches([' ', '│']),
            "11 done · 1 failed"
        );
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        let ascii = script_lines(80, classic);
        assert!(
            ascii
                .iter()
                .all(|l| l.is_ascii() || l.contains('·') || l.contains('×')),
            "{ascii:#?}"
        );
        assert!(
            ascii
                .iter()
                .any(|l| l.starts_with("  |    ` + tools.read ×12  =====x======"))
        );
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
        // In its box: a padding row, the call, and a padding row.
        assert_eq!(text(&on), ["", "  ● Bash: bun run build", ""]);
        assert_eq!(text(&off), ["", "    Bash: bun run build", ""]);
        assert_eq!(on[1].width(), off[1].width());
        let mark = on[1].spans.iter().find(|s| s.content == "●").unwrap();
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

    #[test]
    fn a_status_too_long_for_its_row_wraps_under_the_argument() {
        let rows = vec![Row::Call {
            tool: "Bash".into(),
            argument: "make".into(),
            state: CallState::Failed,
            summary: Some("error: no rule to make target build".into()),
            output: Vec::new(),
        }];
        assert_eq!(
            text(&present(&rows, 0, 30, PLAIN)),
            [
                "  ✗ Bash: make",
                "          error: no rule to",
                "          make target build"
            ]
        );
    }

    #[test]
    fn tools_are_sorted_into_kinds_by_name() {
        let kinds = [
            "Bash",
            "Read",
            "Edit",
            "Grep",
            "Fetch",
            "Agent",
            "Script",
            "mcp_custom",
        ]
        .map(kind);
        assert_eq!(
            kinds,
            [
                Kind::Shell,
                Kind::Read,
                Kind::Edit,
                Kind::Search,
                Kind::Web,
                Kind::Agent,
                Kind::Script,
                Kind::Other
            ]
        );
        assert_eq!(kind("run_code"), Kind::Script);
    }

    #[test]
    fn each_call_sits_in_its_own_light_box() {
        let rows = sample_session();
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
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
        let failed = present(&rows, index, 60, look);
        let done = present(&rows, index + 2, 60, look);
        // The opening blank stays outside the box; every other row is filled
        // to the full width, under every span.
        assert!(failed[0].spans.iter().all(|s| s.style.bg.is_none()));
        for (lines, bg) in [
            (&failed, Color::Rgb(0x2e, 0x22, 0x25)),
            (&done, Color::Rgb(0x25, 0x28, 0x2f)),
        ] {
            for line in &lines[1..] {
                assert_eq!(line.width(), 60, "{line}");
                // Every cell has a background; the box's own frames each row,
                // and a diff row's tint fills its middle.
                assert!(line.spans.iter().all(|s| s.style.bg.is_some()), "{line}");
                assert_eq!(line.spans[0].style.bg, Some(bg), "{line}");
            }
        }
        // Prose is not boxed, and no box is drawn without truecolor.
        let answer = rows.len() - 1;
        assert!(
            present(&rows, answer, 60, look)
                .iter()
                .flat_map(|l| &l.spans)
                .all(|s| s.style.bg.is_none())
        );
        for tones in [Tones::Ansi, Tones::None] {
            let plain = present(&rows, index, 60, Look { tones, ..PLAIN });
            assert!(
                plain
                    .iter()
                    .flat_map(|l| &l.spans)
                    .all(|s| s.style.bg.is_none())
            );
        }
    }

    #[test]
    fn a_boxed_call_keeps_its_text_clear_of_the_right_edge() {
        let rows = vec![Row::Call {
            tool: "Bash".into(),
            argument: "x".into(),
            state: CallState::Done,
            summary: None,
            output: vec!["0123456789abcdefghij".into()],
        }];
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let lines = present(&rows, 0, 20, look);
        // Text wraps two cells short of the edge, so the status moves under
        // the argument; the box still fills the row.
        assert_eq!(
            text(&lines),
            [
                "",
                "  ✓ Bash: x",
                "          1 line",
                "    0123456789abcd",
                "    efghij",
                ""
            ]
        );
        assert!(lines.iter().all(|l| l.width() == 20));
    }

    /// The foreground colour of the span drawing `content` among `lines`.
    fn fg_of(lines: &[Line], content: &str) -> Option<Color> {
        lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == content)
            .unwrap_or_else(|| panic!("no span {content:?}"))
            .style
            .fg
    }

    #[test]
    fn code_is_coloured_where_the_transcript_shows_code() {
        let rows = sample_session();
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let all: Vec<Line> = (0..rows.len())
            .flat_map(|i| present(&rows, i, 84, look))
            .collect();
        let (violet, lime, sky) = (
            Some(Color::Rgb(0xc4, 0xb5, 0xfd)),
            Some(Color::Rgb(0xbe, 0xf2, 0x64)),
            Some(Color::Rgb(0x7d, 0xd3, 0xfc)),
        );
        // The script's source and what it returned.
        assert_eq!(fg_of(&all, "await"), violet);
        assert_eq!(fg_of(&all, "glob"), sky);
        assert_eq!(fg_of(&all, r#""src/m3.ts""#), lime);
        // A shell call's command: the program, its flags, its strings.
        assert_eq!(fg_of(&all, "rg"), sky);
        assert_eq!(fg_of(&all, "'*.ts'"), lime);
        // Program output and prose are never coloured as code.
        let output = all
            .iter()
            .find(|l| l.to_string().contains("controller.ts:45"))
            .unwrap();
        assert!(
            output
                .spans
                .iter()
                .all(|s| s.style.fg != sky && s.style.fg != violet)
        );
        assert!(
            all.iter()
                .any(|l| l.to_string().contains("Two registrations")
                    && l.spans.iter().all(|s| s.style.fg.is_none()))
        );
    }

    #[test]
    fn a_token_split_by_a_wrap_keeps_its_colour_on_every_row() {
        let rows = vec![Row::Call {
            tool: "Bash".into(),
            argument: r#"echo "a long quoted string""#.into(),
            state: CallState::Done,
            summary: None,
            output: Vec::new(),
        }];
        let lines = present(
            &rows,
            0,
            24,
            Look {
                tones: Tones::TrueColor,
                ..PLAIN
            },
        );
        assert_eq!(
            text(&lines),
            [
                "",
                "  ✓ Bash: echo \"a long",
                "          quoted",
                "          string\"",
                ""
            ]
        );
        let lime = Some(Color::Rgb(0xbe, 0xf2, 0x64));
        assert_eq!(fg_of(&lines, "\"a long"), lime);
        assert_eq!(fg_of(&lines, "quoted"), lime);
        assert_eq!(fg_of(&lines, "string\""), lime);
        assert_eq!(fg_of(&lines, "echo"), Some(Color::Rgb(0x7d, 0xd3, 0xfc)));
    }

    #[test]
    fn a_dotted_rule_opens_each_turn_after_the_first_row() {
        let rows = sample_session();
        let all: Vec<String> = (0..rows.len())
            .flat_map(|i| text(&present(&rows, i, 40, PLAIN)))
            .collect();
        let user = all
            .iter()
            .position(|l| l.starts_with("> Find where"))
            .unwrap();
        assert_eq!(all[user - 2], format!("  {}", "┄".repeat(36)));
        assert_eq!(all[user - 1], "");
        assert_eq!(
            all.iter().filter(|l| l.contains('┄')).count(),
            3,
            "one per turn"
        );
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        assert_eq!(rule(10, classic).to_string(), "  ------");
    }

    #[test]
    fn exit_codes_and_interruptions_read_as_tags() {
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let quiet = dim();
        let one = |summary| status_spans(summary, quiet, look);
        let bg = |spans: Vec<Span<'static>>| spans[0].style.bg;
        assert_eq!(one("exit 0")[0].content, " exit 0 ");
        assert_eq!(bg(one("exit 0")), Some(Color::Rgb(0x17, 0x33, 0x22)));
        assert_eq!(bg(one("exit 127")), Some(Color::Rgb(0x3f, 0x1d, 0x22)));
        assert_eq!(bg(one("interrupted")), Some(Color::Rgb(0x3a, 0x34, 0x16)));
        // Anything else stays text.
        assert_eq!(one("412 lines")[0].content, "412 lines");
        assert_eq!(bg(one("exit code")), None);
        // The tag keeps its padding, and its width, without colour.
        let plain = tag("exit 1", TagTone::Red, Tones::None);
        assert_eq!(
            (plain.content.as_ref(), plain.style),
            (" exit 1 ", Style::new())
        );
        assert_eq!(
            tag("x", TagTone::Blue, Tones::Ansi).style.fg,
            Some(Color::Blue)
        );
    }

    #[test]
    fn the_welcome_title_carries_the_palette_when_it_fits() {
        let rows = sample_session();
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let title = |width, look| text(&present(&rows, 0, width, look))[0].clone();
        let strip = format!(
            "{} {}",
            "█".repeat(2 * SWATCHES.len()),
            "█".repeat(2 * GREYS.len())
        );
        assert_eq!(title(80, look), format!("  Bake · Rust preview  {strip}"));
        // Too narrow, without colour, or with the classic frame: the title alone.
        assert_eq!(title(40, look), "  Bake · Rust preview");
        assert_eq!(title(80, PLAIN), "  Bake · Rust preview");
        assert_eq!(
            title(
                80,
                Look {
                    classic: true,
                    ..look
                }
            ),
            "  Bake · Rust preview"
        );
    }

    fn edit_index(rows: &[Row]) -> usize {
        rows.iter()
            .position(|r| matches!(r, Row::Call { tool, .. } if tool == "Edit"))
            .unwrap()
    }

    /// The style of the cell at column `x` of `line`.
    fn cell(line: &Line, x: usize) -> Style {
        let mut at = 0;
        line.spans
            .iter()
            .find(|s| {
                at += s.width();
                at > x
            })
            .map(|s| s.style)
            .unwrap()
    }

    #[test]
    fn an_edit_is_a_numbered_diff_with_its_changes_marked() {
        let rows = sample_session();
        let index = edit_index(&rows);
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let lines = present(&rows, index, 80, look);
        assert_eq!(
            text(&lines),
            [
                "",
                "",
                "  ✓ Edit: src/parser.ts  +1 -1",
                "       ⋯ 40 unmodified lines",
                "    41     const fields = split(line);",
                "    42 ▎   if (quote) fields.push(rest);",
                r#"    42 ▎   if (quote) throw new SyntaxError("unterminated quote");"#,
                "    43     return fields;",
                "",
            ]
        );
        let head = &lines[2];
        assert!(
            cell(head, 4).add_modifier.contains(Modifier::BOLD),
            "tool name"
        );
        assert!(
            cell(head, 10).add_modifier.contains(Modifier::DIM),
            "directory"
        );
        let (red, green) = (
            Some(Color::Rgb(0xef, 0x44, 0x44)),
            Some(Color::Rgb(0x22, 0xc5, 0x5e)),
        );
        let (removed, added, context) = (&lines[5], &lines[6], &lines[4]);
        // Numbers dim on context, in the change's colour on a changed line;
        // the bar in the change's colour.
        assert!(cell(context, 4).add_modifier.contains(Modifier::DIM));
        assert_eq!((cell(removed, 4).fg, cell(removed, 7).fg), (red, red));
        assert_eq!((cell(added, 4).fg, cell(added, 7).fg), (green, green));
        // The line is tinted; what changed takes the stronger tint; the
        // code keeps its syntax colour.
        let line_tint = Some(Color::Rgb(0x21, 0x3a, 0x2c));
        let strong = Some(Color::Rgb(0x2a, 0x5e, 0x3f));
        let at = |needle: &str| added.to_string().find(needle).unwrap();
        assert_eq!(cell(added, at("if")).bg, line_tint);
        assert_eq!(cell(added, at("throw")).bg, strong);
        assert_eq!(
            cell(added, at("throw")).fg,
            Some(Color::Rgb(0xc4, 0xb5, 0xfd))
        );
        assert_eq!(
            cell(added, 79).bg,
            Some(Color::Rgb(0x25, 0x28, 0x2f)),
            "the box frames it"
        );
        assert_eq!(
            cell(removed, removed.to_string().find("push").unwrap()).bg,
            Some(Color::Rgb(0x6b, 0x2f, 0x37))
        );
        assert_eq!(cell(context, 20).bg, Some(Color::Rgb(0x25, 0x28, 0x2f)));
    }

    #[test]
    fn without_colour_a_diff_marks_changes_with_signs() {
        let rows = sample_session();
        let index = edit_index(&rows);
        let plain = text(&present(&rows, index, 80, PLAIN));
        assert_eq!(plain[4], "  │ 42 -   if (quote) fields.push(rest);");
        assert_eq!(
            plain[5],
            r#"  │ 42 +   if (quote) throw new SyntaxError("unterminated quote");"#
        );
        let classic = text(&present(
            &rows,
            index,
            80,
            Look {
                classic: true,
                tones: Tones::Ansi,
                lit: true,
            },
        ));
        assert!(classic[4].starts_with("  | 42 -"));
        // Sixteen colours: the changed line takes its colour, the changed
        // part is bold.
        let ansi = present(
            &rows,
            index,
            80,
            Look {
                tones: Tones::Ansi,
                ..PLAIN
            },
        );
        let added = &ansi[5];
        let x = added.to_string().find("throw").unwrap();
        assert_eq!(cell(added, x).fg, Some(Color::Green));
        assert!(cell(added, x).add_modifier.contains(Modifier::BOLD));
        assert!(
            !cell(added, added.to_string().find("if").unwrap())
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn a_wide_edit_reads_side_by_side_with_each_side_numbered() {
        let rows = sample_session();
        let index = edit_index(&rows);
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        // 104 columns leave the call 102 cells; each side takes 47.
        let wide = present(&rows, index, 104, look);
        let side =
            |left: &str, right: &str| format!("    {left:<47} │ {right}").trim_end().to_owned();
        assert_eq!(
            text(&wide)[3..8],
            [
                "       ⋯ 40 unmodified lines".to_owned(),
                side(
                    "41     const fields = split(line);",
                    "41     const fields = split(line);"
                ),
                side(
                    "42 ▎   if (quote) fields.push(rest);",
                    "42 ▎   if (quote) throw new"
                ),
                side("", r#"   ▎ SyntaxError("unterminated quote");"#),
                side("43     return fields;", "43     return fields;"),
            ]
        );
        assert!(
            wide[1..].iter().all(|l| l.width() == 104),
            "the box fills every row"
        );
        // The removed side keeps its tint beside the added side's wrap.
        let red = Some(Color::Rgb(0x3d, 0x25, 0x29));
        assert_eq!(cell(&wide[6], 20).bg, red);
        // The wrapped part of the added line is all changed text.
        assert_eq!(cell(&wide[6], 60).bg, Some(Color::Rgb(0x2a, 0x5e, 0x3f)));
        // Narrower, the same edit is unified.
        let narrow = text(&present(&rows, index, 100, look));
        assert_eq!(narrow[5], "    42 ▎   if (quote) fields.push(rest);");
    }

    #[test]
    fn a_long_diff_folds_after_its_row_limit() {
        let output: Vec<String> = (0..30).map(|i| format!("+line {i}")).collect();
        let rows = vec![Row::Call {
            tool: "Write".into(),
            argument: "notes.txt".into(),
            state: CallState::Done,
            summary: None,
            output,
        }];
        let lines = text(&present(&rows, 0, 80, PLAIN));
        assert_eq!(lines[0], "  ✓ Write: notes.txt  +30");
        assert_eq!(lines.len(), 1 + DIFF_ROWS + 1);
        assert_eq!(lines.last().unwrap(), "  │    ⋯ 14 more lines");
        assert_eq!(lines[1], "  │  1 + line 0");
        assert_eq!(lines[DIFF_ROWS], "  │ 16 + line 15");
    }

    #[test]
    fn the_thumb_shows_the_visible_share_and_where_it_is() {
        let mut t = session(80, 10);
        let total = t.total();
        assert!(total > 40, "{total}");
        let track = 10;
        // Following: at the bottom, as long as the visible share.
        let bottom = t.thumb(track).unwrap();
        assert_eq!(bottom.len, (track * 10 / total).max(1));
        assert_eq!(bottom.start + bottom.len, track);
        // At the start: at the top.
        t.to_start();
        assert_eq!(t.thumb(track).unwrap().start, 0);
        assert_eq!(t.offset(), 0);
        // A jump lands where asked, and a jump past the end follows output.
        t.jump(12);
        assert_eq!(t.offset(), 12);
        assert!(!t.following());
        t.jump(total);
        assert!(t.following());
        // A click at the track's foot follows; at its head, the start.
        t.jump(t.offset_at(track - 1, track));
        assert!(t.following());
        t.jump(t.offset_at(0, track));
        assert_eq!(t.offset(), 0);
        // A short history has no scrollbar.
        let mut short = Transcript::new(vec![Row::Answer("only".into())]);
        short.resize(80, 10, PLAIN);
        assert_eq!(short.thumb(10), None);
    }

    #[test]
    fn a_changed_row_is_measured_again() {
        let mut t = session(80, 10);
        let before = t.total();
        let last = t.rows.len() - 1;
        t.rows[last] = Row::Answer("short".into());
        t.touched(last);
        t.resize(80, 10, PLAIN);
        assert!(t.total() < before);
        t.rows.push(Row::Answer("more".into()));
        t.resize(80, 10, PLAIN);
        assert_eq!(t.heights.len(), t.rows.len());
    }

    #[test]
    fn shell_output_takes_the_colours_its_command_would_print() {
        let rows = sample_session();
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let all: Vec<Line> = (0..rows.len())
            .flat_map(|i| present(&rows, i, 84, look))
            .collect();
        // A search's path is magenta and its line number green.
        assert_eq!(
            fg_of(&all, "packages/app/src/controller.ts"),
            Some(Color::Rgb(0xf0, 0xab, 0xfc))
        );
        assert_eq!(fg_of(&all, "45"), Some(Color::Rgb(0x86, 0xef, 0xac)));
        // A test run's failure is red and bold, a zero count recedes.
        let fail = all
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == "1 fail")
            .unwrap();
        assert_eq!(fail.style.fg, Some(Color::Rgb(0xf8, 0x71, 0x71)));
        assert!(fail.style.add_modifier.contains(Modifier::BOLD));
        let zero = all
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == "0 fail")
            .unwrap();
        assert!(zero.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn output_never_carries_an_escape_and_only_a_shell_keeps_its_colour() {
        let call = |tool: &str| Row::Call {
            tool: tool.into(),
            argument: "x".into(),
            state: CallState::Done,
            summary: None,
            output: vec!["\u{1b}[31mred\u{1b}[0m\u{1b}[2J\tend".into()],
        };
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        for (tool, coloured) in [("Bash", true), ("Read", false)] {
            let rows = vec![call(tool)];
            let lines = present(&rows, 0, 40, look);
            assert_eq!(lines[2].to_string().trim_end(), "    red    end", "{tool}");
            let red = lines[2]
                .spans
                .iter()
                .find(|s| s.content == "red")
                .map(|s| s.style.fg);
            assert_eq!(red == Some(Some(Color::Indexed(1))), coloured, "{tool}");
        }
    }
}
