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
//! Each kind of tool's card ([`Kind`]) previews its output in the shape
//! that suits it: a shell command's first and last lines, an edit's diff, a
//! write's content as added lines, a search's matches under their files
//! ([`crate::cards`]), a listing's first entries, and a web search's results
//! by title and host. A read or a fetch says what came back in its status
//! and draws no content. Output that is one JSON object or array is laid
//! out a member a line ([`crate::json`]) and coloured as code. A failed
//! call's preview keeps more of its end, where an error says what went
//! wrong. A tool the card knows no argument of shows its arguments as quiet
//! `key value` pairs, and a call whose whole output was saved to a file
//! names that file.
//!
//! [`Detail::Full`], which Ctrl+O toggles, draws every card whole instead:
//! every line wrapped in full, a whole diff, every match and entry, a read's
//! content numbered and coloured, and a fetch's text, each card within
//! [`EXPANDED_ROWS`] rows. Pi's `app.tools.expand` does the same for its
//! tool rows.
//!
//! The viewport presents only the rows it visits: following output, it
//! measures from the newest row up; reading, from its anchor down. Neither
//! measures the whole history.

use std::ops::Range;
use std::time::Duration;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::activity::Tones;
use crate::cards;
use crate::codemode::{self, Codemode, NestedCall, NestedStatus};
use crate::copy;
use crate::diff::{self, Change, DiffLine, DiffRow, SplitRow};
use crate::editor;
use crate::json;
use crate::markdown;
use crate::shell_output::{self, Family, Styled};
use crate::syntax::{self, Carry, Lang};

/// Column a user's words, reasoning, answers, and call marks start at.
pub const RAIL: usize = 2;
/// Output lines a call's preview draws, first and last, around a count of
/// the rest; the TypeScript `resultLines` default.
pub const RESULT_LINES: usize = 4;
/// Rows one output line of a call's preview wraps to before it is cut with
/// `…`, so one long line, such as minified JSON, never fills the screen.
pub const RESULT_LINE_ROWS: usize = 3;
/// Rows a call's argument, such as a long or multi-line shell command,
/// draws before the rest folds into a `+N lines` count at the right of its
/// last row, as the composer counts the rows it hides on its edges.
pub const ARGUMENT_ROWS: usize = 6;
/// Lines of overlap a page keeps, so the reader does not lose their place.
pub const PAGE_OVERLAP: usize = 4;
/// Output lines a failed call's preview draws, its first [`FAILED_HEAD`]
/// and the rest from its end around a count, since an error usually ends
/// the output.
pub const FAILED_LINES: usize = 8;
/// Lines from its start a failed call's preview draws.
pub const FAILED_HEAD: usize = 2;
/// Rows one card draws expanded before the rest is counted, so one huge
/// output never costs more than this to draw.
pub const EXPANDED_ROWS: usize = 400;

/// How much of each call's output the transcript draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Detail {
    /// Each card's preview: a few lines, the rest counted.
    #[default]
    Preview,
    /// Each card whole, within [`EXPANDED_ROWS`] rows.
    Full,
}

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
    /// The live terminal's opening header: Bake, its version, where the
    /// session works, whether it is new, and what it may do.
    Banner(Banner),
    /// A note that asks for the user's attention, such as a published
    /// update: a yellow tag, then its text.
    Notice(Notice),
    /// The user's words that started a turn.
    User(String),
    /// The model's working-out, dim and italic.
    Reasoning(String),
    /// The model's answer.
    Answer(String),
    /// A failure the session reports, such as a provider error, drawn red.
    Error(String),
    /// A note from the session itself, such as where it is saved, drawn dim.
    Info(String),
    /// Where the conversation was compacted: a ruled mark in the
    /// compaction blue with the context before and after, and notes under
    /// it.
    Compacted(Compacted),
    /// A tool call and its result as one block. How `output` reads depends
    /// on the tool's [`Kind`]: an edit's is a unified diff; a write's is the
    /// content it writes, each line opening with `+`; a search's is the
    /// `grep` tool's `path:12: text` and `path-12- text` lines; a listing's
    /// is one path a line; a web search's is one result a line, its title,
    /// a space, and its address; a read's is the file's text and a fetch's
    /// the page's, drawn only expanded; any other's, and any failed call's,
    /// is text.
    Call {
        tool: String,
        argument: String,
        /// The arguments of a tool the card knows no argument of, such as an
        /// MCP tool's, as `key value` pairs drawn after `argument`: each key
        /// dim, each value as given, on one line.
        fields: Vec<(String, String)>,
        /// The background job or subagent the call started or reached, such
        /// as `bash-1`, drawn as a blue tag before its status.
        id: Option<String>,
        state: CallState,
        /// The card's headline on the head, such as `3 matches` or `exit 1`.
        /// Its parts, split by [`copy::STATUS_SEP`], each read alone: an
        /// exit code, an interruption, or a stop is a tag.
        summary: Option<String>,
        output: Vec<String>,
        /// The file holding the whole output when the tool cut what it
        /// returned, drawn dim under the output.
        full_output_path: Option<String>,
    },
    /// A `codemode` call: its script, the calls the script made, and what
    /// went back to the model.
    Codemode(Codemode),
}

/// What the live terminal's [`Row::Banner`] shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    /// The running version, such as `0.4.2`.
    pub version: String,
    /// The working directory, with the user's home shortened to `~`.
    pub directory: String,
    /// The session in plain words, such as `new` or `resumed · 6 prompts ·
    /// 2h ago`.
    pub session: String,
    /// What the session may do, as tags: its sandbox mode, its approval
    /// policy, and whether it is offline. None leaves the line out.
    pub access: Vec<Pill>,
    /// The tip shown under the tags for a while after startup, which the
    /// view sets and clears ([`crate::tips`]); `None` leaves the line out.
    pub tip: Option<String>,
}

/// One tag in the banner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pill {
    pub text: String,
    pub tone: TagTone,
}

/// What a [`Row::Compacted`] marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compacted {
    /// What happened, such as `Compacted` or `Auto-compacted`.
    pub title: String,
    /// The context before it, in tokens.
    pub before: u64,
    /// The context after it, when measured.
    pub after: Option<u64>,
    /// Dim notes under the mark, each its own paragraph, such as how much
    /// of the summary's prompt the cache served.
    pub notes: Vec<String>,
}

/// What a [`Row::Notice`] says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The tag before it, such as `update`.
    pub tag: String,
    pub text: String,
}

impl Row {
    /// A branch summary in a replayed transcript, Pi's collapsed
    /// `BranchSummaryMessageComponent`.
    pub fn branch_summary() -> Self {
        Self::Info(copy::BRANCH_SUMMARY_ROW.to_owned())
    }

    /// The note of a cancelled `/tree` branch summary, Pi's status after
    /// `abortBranchSummary`.
    pub fn branch_summary_cancelled() -> Self {
        Self::Info(copy::BRANCH_SUMMARY_CANCELLED_ROW.to_owned())
    }
}

/// The fixed sample session the preview opens with; nothing in it ran.
pub fn sample_session() -> Vec<Row> {
    let call =
        |tool: &str, argument: &str, state, summary: Option<&str>, output: &[&str]| Row::Call {
            tool: tool.into(),
            argument: argument.into(),
            fields: Vec::new(),
            id: None,
            state,
            summary: summary.map(str::to_owned),
            output: output.iter().map(|line| (*line).to_owned()).collect(),
            full_output_path: None,
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
        call(
            "Read",
            "packages/app/src/controller.ts:44-53",
            CallState::Done,
            Some("10 lines"),
            &[
                "  ctx.effect(() => {",
                "    commands.register(open);",
                "    return () => commands.unregister(open);",
                "  });",
                "",
                "  // Closing waits for the menu to settle.",
                "",
                "  ctx.effect(() => {",
                "    commands.register(close);",
                "    return () => commands.unregister(close);",
            ],
        ),
        call(
            "Grep",
            "ctx.effect in packages/app",
            CallState::Done,
            Some("6 matches · 3 files"),
            &[
                "src/controller.ts:44:   ctx.effect(() => {",
                "src/controller.ts:51:   ctx.effect(() => {",
                "src/plugin.ts:27:   const stop = ctx.effect(watch);",
                "src/plugin.ts:40:   ctx.effect(() => stop());",
                "src/menu.ts:18:   ctx.effect(() => menu.attach(ctx));",
                "src/menu.ts:33:     ctx.effect(item.dispose);",
            ],
        ),
        Row::Answer(
            "Two registrations, both through ctx.effect, so each one is disposed with the plugin that made it."
                .into(),
        ),
        Row::User("Find TODO comments in the source files".into()),
        sample_script(),
        Row::Call {
            tool: format!("github{}search_issues", copy::MCP_TOOL_SEP),
            argument: String::new(),
            fields: [
                ("repo", "acme/app"),
                ("query", "TODO in:title"),
                ("state", "open"),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .to_vec(),
            id: None,
            state: CallState::Done,
            summary: None,
            output: vec![
                r#"{"number":214,"title":"Resolve the TODO in src/m3.ts","state":"open"}"#.into(),
            ],
            full_output_path: None,
        },
        Row::Answer(
            "Two files have TODOs: src/m3.ts, which #214 already tracks, and src/m9.ts. src/m5.ts could not be read: permission denied."
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
                "(pass) reads a header row",
                "(pass) skips a comment line",
                "error: expect(received).toThrow()",
                "Received function did not throw",
                "    at <anonymous> (tests/parser.test.ts:41:32)",
                "(fail) rejects an unterminated quote",
                " 5 pass",
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
            "Write",
            "tests/quote.test.ts",
            CallState::Done,
            None,
            &[
                "+import { expect, test } from \"bun:test\";",
                "+import { parse } from \"../src/parser\";",
                "+",
                "+test(\"rejects an unterminated quote\", () => {",
                "+  expect(() => parse('a,\"b')).toThrow(SyntaxError);",
                "+});",
                "+",
                "+test(\"keeps a doubled quote\", () => {",
                "+  expect(parse('\"a\"\"b\"')).toEqual(['a\"b']);",
                "+});",
            ],
        ),
        call(
            "Bash",
            "bun test tests",
            CallState::Done,
            Some("exit 0"),
            &[
                "bun test v1.3.0",
                "tests/parser.test.ts:",
                "(pass) splits fields",
                "(pass) keeps quoted commas",
                "(pass) trims trailing blanks",
                "(pass) reads a header row",
                "(pass) skips a comment line",
                "(pass) rejects an unterminated quote",
                "tests/quote.test.ts:",
                "(pass) rejects an unterminated quote",
                "(pass) keeps a doubled quote",
                " 8 pass",
                " 0 fail",
            ],
        ),
        Row::Answer(
            "One test failed because an unterminated quote was accepted. The parser now rejects it before splitting."
                .into(),
        ),
    ]
}

/// A sample `codemode` call that reads every source file, one of which it
/// may not read: it catches that call's error and logs it, so the model
/// learns of it without the script failing.
fn sample_script() -> Row {
    let read = |index: usize| NestedCall {
        name: "read".into(),
        args: format!(r#"{{"path":"src/m{index}.ts"}}"#),
        status: if index == 5 {
            NestedStatus::Error
        } else {
            NestedStatus::Ok
        },
        duration: Some(Duration::from_millis(2 + index as u64 % 3)),
        error: (index == 5).then(|| "Permission denied".into()),
    };
    let mut calls = vec![NestedCall {
        name: "find".into(),
        args: r#"{"pattern":"src/**/*.ts"}"#.into(),
        status: NestedStatus::Ok,
        duration: Some(Duration::from_millis(12)),
        error: None,
    }];
    calls.extend((0..12).map(read));
    Row::Codemode(Codemode {
        code: [
            "const files = (await tools.find({ pattern: \"src/**/*.ts\" })).split(\"\\n\");",
            "const found = [];",
            "for (const path of files) {",
            "  try {",
            "    if ((await tools.read({ path })).includes(\"TODO\")) found.push(path);",
            "  } catch (error) {",
            "    console.log(`skipped ${path}: ${error.message}`);",
            "  }",
            "}",
            "return found;",
        ]
        .join("\n"),
        state: CallState::Done,
        summary: None,
        calls,
        output: [
            r#"["src/m3.ts","src/m9.ts"]"#,
            "<console_output>",
            "skipped src/m5.ts: Permission denied",
            "</console_output>",
        ]
        .map(str::to_owned)
        .to_vec(),
        full_output_path: None,
        wall_time: Some(Duration::from_millis(400)),
    })
}

/// Rows that open `row`, given the row before it: none for the first row, a
/// blank after the last turn's end then a dashed rule right above a user's
/// words, and a blank before any other row, so each call's box stands apart
/// from the next.
fn opening(previous: Option<&Row>, row: &Row, width: usize, look: Look) -> Vec<Line<'static>> {
    match (previous, row) {
        (None, _) => Vec::new(),
        (Some(_), Row::User(_)) => vec![Line::default(), rule(width, 0, look)],
        (Some(_), _) => vec![Line::default()],
    }
}

/// A faint dashed rule across `width`, `margin` cells in from each edge, as
/// a printed catalogue rules off a section. Each cell's ASCII hyphen is a
/// short dash with a gap either side, so a run reads as a fine dashed line
/// on every terminal and font.
pub fn rule(width: usize, margin: usize, look: Look) -> Line<'static> {
    let cells = width.saturating_sub(2 * margin);
    Line::from(vec![
        pad(margin),
        Span::styled(look.marks().rule.repeat(cells), faint(look.tones)),
    ])
}

/// `Compacted · 182.3k → 21.1k tokens - - -`: the title in the compaction
/// blue, the counts dim, and a faint rule to `width`, as the turn's rule
/// rules off a prompt; then each note, dim, under the title. A head too
/// wide for a rule wraps without one.
fn present_compacted(mark: &Compacted, width: usize, look: Look) -> Vec<Line<'static>> {
    // A note may carry a provider's error text: no escape sequence or
    // control character reaches the terminal.
    let clean = |text: &str| {
        text.lines()
            .map(|line| shell_output::parse_sgr(line, Tones::None).0)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let arrow = if look.classic { "->" } else { "→" };
    let counts = match mark.after {
        Some(after) => format!(
            "{} {arrow} {} tokens",
            crate::status::format_tokens(mark.before),
            crate::status::format_tokens(after)
        ),
        None => format!("{} tokens", crate::status::format_tokens(mark.before)),
    };
    let title = clean(&mark.title);
    let blue = look
        .tones
        .style(crate::activity::Hue::Compacting, 0.0)
        .add_modifier(Modifier::BOLD);
    let head = format!("{title} · {counts}");
    let mut lines = Vec::new();
    let room = width.saturating_sub(RAIL);
    if head.width() + 2 <= room {
        let rest = room - head.width() - 1;
        lines.push(Line::from(vec![
            pad(RAIL),
            Span::styled(title, blue),
            Span::styled(format!(" · {counts} "), dim()),
            Span::styled(look.marks().rule.repeat(rest), faint(look.tones)),
        ]));
    } else {
        lines.extend(hang(
            &head,
            width,
            RAIL,
            vec![pad(RAIL)],
            vec![pad(RAIL)],
            blue,
        ));
    }
    for note in &mark.notes {
        lines.extend(hang(
            &clean(note),
            width,
            RAIL,
            vec![pad(RAIL)],
            vec![pad(RAIL)],
            dim(),
        ));
    }
    lines
}

/// Fainter than [`dim`]: the darkest palette grey on a truecolor terminal,
/// and dimmed dark grey, or dimmed text under `NO_COLOR`, elsewhere.
fn faint(tones: Tones) -> Style {
    match tones {
        Tones::TrueColor => Style::new().fg(Color::Rgb(0x37, 0x41, 0x51)),
        Tones::Ansi => dim().fg(Color::DarkGray),
        Tones::None => dim(),
    }
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

/// The tag a status part reads as: green for a clean exit, red for a
/// failing one, and yellow for an interruption, a stop, or a lost job.
fn status_tone(part: &str) -> Option<TagTone> {
    match part.strip_prefix("exit ") {
        Some("0") => Some(TagTone::Green),
        Some(code) if code.parse::<i32>().is_ok() => Some(TagTone::Red),
        _ if [copy::INTERRUPTED_NOTE, copy::JOB_STOPPED, copy::JOB_LOST].contains(&part) => {
            Some(TagTone::Yellow)
        }
        _ => None,
    }
}

/// A call's status as drawn: each part split by [`copy::STATUS_SEP`] that
/// has a [`status_tone`] as a tag, and the rest as text, so `already
/// finished · exit 0` ends in a green tag. A status with no such part is
/// one run of text.
pub(crate) fn status_spans(summary: &str, quiet: Style, look: Look) -> Vec<Span<'static>> {
    let parts: Vec<(&str, Option<TagTone>)> = summary
        .split(copy::STATUS_SEP)
        .map(|part| (part, status_tone(part)))
        .collect();
    if parts.iter().all(|(_, tone)| tone.is_none()) {
        return vec![Span::styled(summary.to_owned(), quiet)];
    }
    let mut spans = Vec::new();
    for (index, (part, tone)) in parts.iter().enumerate() {
        if index > 0 {
            // A tag's padding stands in for the separator's space beside it.
            let before = if parts[index - 1].1.is_some() {
                ""
            } else {
                " "
            };
            let after = if tone.is_some() { "" } else { " " };
            spans.push(Span::styled(format!("{before}·{after}"), quiet));
        }
        spans.push(match tone {
            Some(tone) => tag(part, *tone, look.tones),
            None => Span::styled((*part).to_owned(), quiet),
        });
    }
    spans
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
pub(crate) fn gutter(look: Look) -> &'static str {
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
/// filled with `bg` across the full `width`. A call of one line, such as a
/// read or a search that found nothing to show, stays one tinted row, so a
/// run of them reads as a list rather than a stack of boxes.
fn card(lines: &mut Vec<Line<'static>>, width: usize, bg: Color) {
    if lines.len() > 1 {
        lines.insert(0, Line::default());
        lines.push(Line::default());
    }
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
    /// A call a script made that its end, a timeout, or an abort cut off.
    pub cancelled: &'static str,
    /// The dash of the rule that opens a user's turn: ASCII in both sets,
    /// since box-drawing dashes are missing from many console fonts.
    pub rule: &'static str,
}

pub const ROUND_MARKS: Marks = Marks {
    user: ">",
    done: "✓",
    failed: "✗",
    running: "●",
    gutter: "│",
    more: "⋯",
    cancelled: "⊘",
    rule: "-",
};

pub const CLASSIC_MARKS: Marks = Marks {
    user: ">",
    done: "+",
    failed: "x",
    running: "*",
    gutter: "|",
    more: "...",
    cancelled: "/",
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
/// script's source and the calls it made, and the counts of what is folded
/// away. Column [`RAIL`] holds the call's structure: its state mark and the
/// gutter.
pub const BODY: usize = 4;
/// Cells a call's text keeps clear of the box's right edge.
pub const BOX_PAD: usize = 2;

/// `text` wrapped at `width` less `indent`; each row after the first opens
/// with `rest`, the first with `first`.
pub(crate) fn hang(
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

/// How many rows [`hang`] and [`hang_runs`] wrap `text` into, without
/// drawing them.
pub(crate) fn hang_rows(text: &str, width: usize, indent: usize) -> usize {
    let room = width.saturating_sub(indent).max(1);
    editor::layout(text, 0, room + 1).rows.len()
}

/// `text` with each tab turned into the spaces to its next stop, so the
/// lexer's byte ranges and the drawn cells agree.
pub(crate) fn expand_tabs(text: &str) -> String {
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
pub(crate) fn code_runs(
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
pub(crate) fn hang_runs(
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
            clip_line(Line::from(spans), width)
        })
        .collect()
}

/// `line` cut to `width` cells. A wrapped row can run past its room when
/// the spaces a wrap leaves at its end are followed by a character of no
/// width, such as U+200B, which `trim_end` keeps.
fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let mut room = width;
    let mut spans = Vec::with_capacity(line.spans.len());
    for span in line.spans {
        if room == 0 {
            break;
        }
        let cells = span.content.width();
        if cells <= room {
            room -= cells;
            spans.push(span);
            continue;
        }
        let mut kept = String::new();
        for grapheme in span.content.graphemes(true) {
            let cells = grapheme.width();
            if cells > room {
                break;
            }
            room -= cells;
            kept.push_str(grapheme);
        }
        spans.push(Span::styled(kept, span.style));
        break;
    }
    Line::from(spans)
}

pub(crate) fn pad(cells: usize) -> Span<'static> {
    Span::raw(" ".repeat(cells))
}

pub(crate) fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// A palette colour on a truecolor terminal, its nearest ANSI colour
/// otherwise, and none under `NO_COLOR`.
pub(crate) fn colour(tones: Tones, rgb: (u8, u8, u8), ansi: Color) -> Style {
    match tones {
        Tones::TrueColor => Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        Tones::Ansi => Style::new().fg(ansi),
        Tones::None => Style::new(),
    }
}

pub(crate) fn accent(tones: Tones) -> Style {
    colour(tones, (0x0e, 0xa5, 0xe9), Color::Cyan)
}

/// The lines `rows[index]` draws at `width`, its opening blank included,
/// each call previewed.
pub fn present(rows: &[Row], index: usize, width: usize, look: Look) -> Vec<Line<'static>> {
    present_detail(rows, index, width, look, Detail::Preview)
}

/// [`present`] with each call's output drawn as `detail` says.
pub fn present_detail(
    rows: &[Row],
    index: usize,
    width: usize,
    look: Look,
    detail: Detail,
) -> Vec<Line<'static>> {
    present_row(rows, index, width, look, detail, false).0
}

/// [`present_detail`], with [`copy::EXPAND_HINT`] after the card's last
/// count when `hinted`, and whether the row is a card with a count the hint
/// can follow.
fn present_row(
    rows: &[Row],
    index: usize,
    width: usize,
    look: Look,
    detail: Detail,
    hinted: bool,
) -> (Vec<Line<'static>>, bool) {
    let row = &rows[index];
    let marks = look.marks();
    let mut counted = false;
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
    // Prose keeps the rail's width clear on the right too, so it ends where
    // a call's boxed text does instead of against the scrollbar.
    let prose = width.saturating_sub(RAIL).max(RAIL + 1);
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
                lines.extend(hang(paragraph, prose, RAIL, rail(), rail(), dim()));
            }
        }
        Row::Banner(banner) => lines.extend(present_banner(banner, prose, look)),
        Row::Notice(notice) => {
            let label = tag(&notice.tag, TagTone::Yellow, look.tones);
            let indent = RAIL + label.width() + 1;
            lines.extend(hang(
                &notice.text,
                prose,
                indent,
                vec![pad(RAIL), label, pad(1)],
                vec![pad(indent)],
                Style::new(),
            ));
        }
        // A prompt mark opens the user's words, as they were typed; the
        // rows a long prompt wraps to hang under the text, not the mark.
        Row::User(text) => {
            let mark = vec![Span::styled(format!("{} ", marks.user), accent(look.tones))];
            lines.extend(hang(
                text,
                prose,
                RAIL,
                mark,
                rail(),
                Style::new().add_modifier(Modifier::BOLD),
            ));
        }
        Row::Reasoning(text) => lines.extend(hang(
            text,
            prose,
            RAIL,
            rail(),
            rail(),
            dim().add_modifier(Modifier::ITALIC),
        )),
        Row::Answer(text) => lines.extend(markdown::present(text, prose, look)),
        Row::Error(text) => lines.extend(hang(
            text,
            prose,
            RAIL,
            rail(),
            rail(),
            colour(look.tones, (0xef, 0x44, 0x44), Color::Red),
        )),
        Row::Info(text) => lines.extend(hang(text, prose, RAIL, rail(), rail(), dim())),
        Row::Compacted(mark) => lines.extend(present_compacted(mark, prose, look)),
        Row::Call {
            tool,
            argument,
            fields,
            id,
            state,
            summary,
            output,
            full_output_path,
        } => {
            let mut block = present_call(
                Call {
                    tool,
                    argument,
                    fields,
                    id: id.as_deref(),
                    state: *state,
                    summary: summary.as_deref(),
                    output,
                    full_output_path: full_output_path.as_deref(),
                },
                inner,
                look,
                detail,
            );
            counted = hint_count(&mut block, inner, look, hinted);
            if let Some(bg) = box_colour(*state, look.tones) {
                card(&mut block, width, bg);
            }
            lines.extend(block);
        }
        Row::Codemode(script) => {
            let mut block = codemode::present(script, inner, look, detail);
            counted = hint_count(&mut block, inner, look, hinted);
            if let Some(bg) = box_colour(script.state, look.tones) {
                card(&mut block, width, bg);
            }
            lines.extend(block);
        }
    }
    (lines, counted)
}

/// Whether a card's rows hold a count, such as `⋯ 12 more lines` or `+40
/// above`; with `hinted`, puts a dim [`copy::EXPAND_HINT`] after the last
/// one when it fits on that count's row; otherwise the card is left as it
/// was, so the hint never adds a row.
fn hint_count(block: &mut [Line<'static>], width: usize, look: Look, hinted: bool) -> bool {
    let more = look.marks().more;
    let number = |text: &str| text.starts_with(|c: char| c.is_ascii_digit());
    let counts = |text: &str| {
        text.strip_prefix(more)
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(number)
            || text
                .strip_prefix('+')
                .is_some_and(|rest| number(rest) && rest.ends_with(copy::ABOVE))
    };
    let Some(row) = block.iter_mut().rev().find(|row| {
        row.spans
            .last()
            .is_some_and(|span| span.style == dim() && counts(&span.content))
    }) else {
        return false;
    };
    // A count wrapped onto two rows never has room on its first: the word
    // it wrapped is narrower than the hint.
    let hint = Span::styled(format!("{}{}", copy::STATUS_SEP, copy::EXPAND_HINT), dim());
    if hinted && row.width() + hint.width() <= width {
        row.spans.push(hint);
    }
    true
}

/// The banner's lines: a blank row, the wordmark, its version, and the tagline when it
/// fits, then each detail under a dim label, its value hung beside it, and
/// the access tags, which flow onto more rows when they do not fit.
fn present_banner(banner: &Banner, width: usize, look: Look) -> Vec<Line<'static>> {
    let mut title = vec![pad(RAIL)];
    title.extend(wordmark(look));
    title.push(pad(1));
    title.push(Span::styled(format!("v{}", banner.version), dim()));
    let used: usize = title.iter().map(Span::width).sum();
    let tagline = format!("{}{}", copy::BANNER_SEP, copy::BANNER_TAGLINE);
    if used + tagline.width() <= width {
        title.push(Span::styled(tagline, dim()));
    }
    // A blank row above keeps the title off the terminal's top edge, or
    // the shell prompt above it inline.
    let mut lines = vec![Line::default(), Line::from(title)];
    let [directory, session, access, tip] = copy::BANNER_LABELS;
    let column = copy::BANNER_LABELS
        .iter()
        .map(|label| label.width())
        .max()
        .unwrap_or(0)
        + 2;
    let label = |name: &str| vec![pad(RAIL), Span::styled(format!("{name:<column$}"), dim())];
    for (name, value) in [(directory, &banner.directory), (session, &banner.session)] {
        lines.extend(hang(
            value,
            width,
            RAIL + column,
            label(name),
            vec![pad(RAIL + column)],
            Style::new(),
        ));
    }
    if !banner.access.is_empty() {
        let start = RAIL + column;
        let mut row = label(access);
        let mut at = start;
        for pill in &banner.access {
            let span = tag(&pill.text, pill.tone, look.tones);
            let gap = usize::from(at > start);
            if at > start && at + gap + span.width() > width {
                lines.push(clip_line(Line::from(std::mem::take(&mut row)), width));
                row.push(pad(start));
                at = start;
            } else if gap > 0 {
                row.push(pad(1));
                at += 1;
            }
            at += span.width();
            row.push(span);
        }
        lines.push(clip_line(Line::from(row), width));
    }
    if let Some(text) = &banner.tip {
        lines.extend(hang(
            text,
            width,
            RAIL + column,
            label(tip),
            vec![pad(RAIL + column)],
            dim(),
        ));
    }
    lines
}

/// The gradient [`wordmark`] runs along: amber, orange, then rose, an
/// oven's glow.
const WORDMARK_GLOW: [(u8, u8, u8); 3] =
    [(0xf5, 0x9e, 0x0b), (0xf9, 0x73, 0x16), (0xf4, 0x3f, 0x5e)];

/// "Bake", bold: each letter a step along [`WORDMARK_GLOW`] on a truecolor
/// terminal, the accent on sixteen colours or with the classic frame, and
/// only bold under `NO_COLOR`.
fn wordmark(look: Look) -> Vec<Span<'static>> {
    let bold = Modifier::BOLD;
    if look.tones != Tones::TrueColor || look.classic {
        return vec![Span::styled(
            copy::TITLE,
            accent(look.tones).add_modifier(bold),
        )];
    }
    let letters: Vec<&str> = copy::TITLE.graphemes(true).collect();
    let last = letters.len().saturating_sub(1).max(1);
    letters
        .iter()
        .enumerate()
        .map(|(index, letter)| {
            let (r, g, b) = glow(index, last);
            Span::styled(
                (*letter).to_owned(),
                Style::new().fg(Color::Rgb(r, g, b)).add_modifier(bold),
            )
        })
        .collect()
}

/// The colour `index` of `last` steps along [`WORDMARK_GLOW`].
fn glow(index: usize, last: usize) -> (u8, u8, u8) {
    let spans = WORDMARK_GLOW.len() - 1;
    // Position in the gradient, in units of 1/(last) of one stop.
    let scaled = index.min(last) * spans;
    let stop = (scaled / last).min(spans - 1);
    let within = scaled - stop * last;
    let mix = |from: u8, to: u8| {
        let (from, to) = (i64::from(from), i64::from(to));
        let value = from
            + (to - from) * i64::try_from(within).unwrap_or(0) / i64::try_from(last).unwrap_or(1);
        u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
    };
    let (from, to) = (WORDMARK_GLOW[stop], WORDMARK_GLOW[stop + 1]);
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
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
    /// Writing a whole file; its output is the content, as a diff's added
    /// lines.
    Write,
    /// Searching file contents; its output is the `grep` tool's lines.
    Grep,
    /// Listing paths; its output is one a line.
    List,
    /// Fetching a web page.
    Fetch,
    /// Searching the web; its output is one result a line, its title then
    /// its address.
    WebSearch,
    /// Delegating to a subagent.
    Agent,
    Other,
}

/// The kind of a tool, by its name as the session log or the terminal
/// labels it, a space read as `_`.
pub fn kind(tool: &str) -> Kind {
    match tool.to_ascii_lowercase().replace(' ', "_").as_str() {
        "bash" | "sh" | "zsh" | "shell" | "pwsh" | "powershell" => Kind::Shell,
        "read" | "read_file" | "view" => Kind::Read,
        "edit" | "multiedit" | "apply_patch" => Kind::Edit,
        "write" | "write_file" => Kind::Write,
        "grep" => Kind::Grep,
        "glob" | "find" | "ls" => Kind::List,
        "fetch" | "web_fetch" => Kind::Fetch,
        "websearch" | "web_search" => Kind::WebSearch,
        "agent" | "task" | "spawn" => Kind::Agent,
        _ => Kind::Other,
    }
}

impl Kind {
    /// Whether the argument is a file path, drawn with its directory dim.
    fn takes_path(self) -> bool {
        matches!(self, Self::Read | Self::Edit | Self::Write)
    }
}

/// A call state's mark and its colour: a white dot that blinks while the
/// call runs, a green check when it is done, and a red cross when it failed.
pub(crate) fn mark_of(state: CallState, look: Look) -> (&'static str, Style) {
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
pub(crate) fn script_mark(state: CallState, look: Look) -> (&'static str, Style) {
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

pub(crate) fn red(look: Look) -> Style {
    colour(look.tones, (0xef, 0x44, 0x44), Color::Red)
}

/// A call's head: `lead`, its state mark, the tool name in its column, and
/// the argument, then any fields, wrapped under itself. Wrapped rows open
/// with `rest`, which takes as many cells as `lead`. Returns the rows and
/// the lead of a row that continues under the argument, for [`ride`].
fn head(
    lead: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    call: &Call,
    width: usize,
    look: Look,
) -> (Vec<Line<'static>>, Vec<Span<'static>>) {
    let label = format!("{}:", clean_line(call.tool));
    let column = TOOL_WIDTH.max(label.width() + 1);
    let name = vec![Span::styled(
        label,
        Style::new().add_modifier(Modifier::BOLD),
    )];
    labelled(lead, rest, call, name, column, width, look)
}

/// `text` on one line, with no escape sequence or control character: a
/// line break reads as a space.
fn clean_line(text: &str) -> String {
    shell_output::parse_sgr(&text.replace(['\n', '\r'], " "), Tones::None).0
}

/// `text` with no escape sequence or control character, its tabs turned to
/// the spaces to their stops, and its line breaks kept, so a multi-line
/// command still breaks where it did.
fn clean_text(text: &str) -> String {
    text.split('\n')
        .map(|line| shell_output::parse_sgr(&expand_tabs(line), Tones::None).0)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `argument` then `fields` as one text and the runs that style it: each
/// pair's key dim, its value plain, and pairs set apart by a dim
/// [`copy::STATUS_SEP`], which keeps them apart where dim does not show.
/// Line breaks read as spaces; escapes and control characters are dropped.
fn with_fields(
    argument: &str,
    fields: &[(String, String)],
) -> (String, Vec<(Range<usize>, Style)>) {
    let clean = clean_line;
    let mut text = clean(argument);
    let mut runs = Vec::new();
    for (key, value) in fields {
        if !text.is_empty() {
            runs.push((text.len()..text.len() + copy::STATUS_SEP.len(), dim()));
            text.push_str(copy::STATUS_SEP);
        }
        let key = clean(key);
        runs.push((text.len()..text.len() + key.len(), dim()));
        text.push_str(&key);
        text.push(' ');
        text.push_str(&clean(value));
    }
    (text, runs)
}

/// [`head`] with its label drawn by the caller: `name` padded to `column`
/// cells, then the argument, which reads as the call's [`Kind`] decides,
/// then its fields.
fn labelled(
    lead: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    call: &Call,
    name: Vec<Span<'static>>,
    column: usize,
    width: usize,
    look: Look,
) -> (Vec<Line<'static>>, Vec<Span<'static>>) {
    let (state, kind) = (call.state, kind(call.tool));
    let argument = &clean_text(call.argument);
    let (mark, mark_style) = mark_of(state, look);
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
    if kind.takes_path() && fits && call.fields.is_empty() {
        let split = argument.rfind('/').map_or(0, |i| i + 1);
        first.push(Span::styled(argument[..split].to_owned(), dim()));
        first.push(Span::raw(argument[split..].to_owned()));
        return (vec![Line::from(first)], rest);
    }
    // A shell command reads as code: its program, flags, and strings.
    let (argument, runs) = if call.fields.is_empty() {
        let lang = (kind == Kind::Shell).then_some(Lang::Shell);
        let runs = code_runs(argument, lang, &mut Carry::default(), look.tones);
        (argument.clone(), runs)
    } else {
        with_fields(call.argument, call.fields)
    };
    let mut lines = hang_runs(
        &argument,
        &runs,
        Style::new(),
        width,
        indent,
        first,
        rest.clone(),
    );
    // A long argument keeps its first rows and counts the rest at the right
    // of the last one, so a script passed to a shell never pushes the call
    // off the screen and the count takes no row of its own.
    if lines.len() > ARGUMENT_ROWS {
        let mut hidden = lines.len() - ARGUMENT_ROWS;
        lines.truncate(ARGUMENT_ROWS);
        let mut label = format!("+{}", cards::count(hidden, copy::LINE_NOUNS));
        let fits = |line: &Line, label: &str| line.width() + 2 + label.width() <= width;
        if !lines.last().is_some_and(|last| fits(last, &label)) {
            // No room beside the last row: it gives way to the count.
            lines.pop();
            hidden += 1;
            label = format!("+{}", cards::count(hidden, copy::LINE_NOUNS));
            let mut spans = rest.clone();
            spans.push(Span::styled(label, dim()));
            lines.push(Line::from(spans));
        } else if let Some(last) = lines.last_mut() {
            let fill = width - last.width() - label.width();
            last.spans.push(pad(fill));
            last.spans.push(Span::styled(label, dim()));
        }
    }
    (lines, rest)
}

/// Keeps the first `limit` of `lines`, the last of them ending in a dim
/// `…` in place of its last character, so the cut shows without widening
/// the row.
pub(crate) fn clip_rows(lines: &mut Vec<Line<'static>>, limit: usize) {
    if lines.len() <= limit || limit == 0 {
        return;
    }
    lines.truncate(limit);
    if let Some(last) = lines.last_mut() {
        // The last cell drawn gives way, past any empty span, such as a
        // full row's fill, and any character of no width.
        if let Some(span) = last
            .spans
            .iter_mut()
            .rev()
            .find(|span| span.content.width() > 0)
        {
            let mut text = span.content.to_string();
            let before = text.width();
            while !text.is_empty() && text.width() == before {
                text.pop();
            }
            span.content = text.into();
        }
        last.spans.push(Span::styled("…", dim()));
    }
}

/// A dim note, such as the count of what a body left out, wrapped under
/// `lead`, so it never runs past `width`.
pub(crate) fn note(lead: Vec<Span<'static>>, text: &str, width: usize) -> Vec<Line<'static>> {
    let indent = lead.iter().map(Span::width).sum();
    hang(text, width, indent, lead.clone(), lead, dim())
}

/// The rows of items `0..count`, each drawn by `build` in order, until they
/// fill `limit` rows: the item that reaches the bound is cut there with
/// `…`, and the items after it are counted in the rows `more` draws. Only
/// the items drawn are built. Returns the rows and whether anything was
/// left out.
pub(crate) fn bounded(
    count: usize,
    limit: usize,
    mut build: impl FnMut(usize) -> Vec<Line<'static>>,
    more: impl FnOnce(usize) -> Vec<Line<'static>>,
) -> (Vec<Line<'static>>, bool) {
    let mut lines = Vec::new();
    for index in 0..count {
        lines.extend(build(index));
        if lines.len() >= limit && index + 1 < count {
            clip_rows(&mut lines, limit);
            lines.extend(more(count - index - 1));
            return (lines, true);
        }
    }
    let cut = lines.len() > limit;
    clip_rows(&mut lines, limit);
    (lines, cut)
}

/// Puts a call's status, such as `2 lines`, two cells after the last row's
/// text, so it reads with the call at any width. When it does not fit there,
/// it takes rows of its own opening with `lead`, under the argument, and
/// wraps there rather than being cut: a one-run status in its own style, and
/// one of several runs, such as tags, as dim text.
pub(crate) fn ride(
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
    } else {
        let joined: String = summary.iter().map(|span| span.content.as_ref()).collect();
        lines.extend(hang(
            joined.trim(),
            width,
            lead_cells,
            lead.clone(),
            lead,
            dim(),
        ));
    }
}

/// Cells a call's text area needs before an edit's diff goes side by side,
/// so each side keeps about 46 cells.
pub const SPLIT_MIN: usize = 100;
/// Cells the divider between a split diff's sides takes: the gutter's rule
/// with a space either side, so classic marks draw it in ASCII.
const DIVIDER_CELLS: usize = 3;
/// Diff rows an edit draws before the rest folds into a count.
pub const DIFF_ROWS: usize = 16;
/// Lines of a write's content its card draws before the rest folds into a
/// count: enough to know the file by its opening.
pub const WRITE_ROWS: usize = 6;

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
/// wrapped under itself. Unless `marked`, the mark's column is left out, so
/// code with no change to mark, such as a read's, starts two cells after its
/// number. In a box, a changed line is tinted across the side and the part
/// that changed takes the stronger tint; without one, a changed line takes
/// its colour whole and the changed part is bold. An empty side is a blank
/// row.
fn diff_side(
    line: Option<&DiffLine>,
    cells: usize,
    digits: usize,
    lang: Option<Lang>,
    carry: &mut Carry,
    (look, marked): (Look, bool),
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
    let (first, rest) = if marked {
        (
            vec![
                Span::styled(format!("{number:>digits$} "), number_style),
                mark.clone(),
                pad(1),
            ],
            vec![pad(digits + 1), mark, pad(1)],
        )
    } else {
        (
            vec![Span::styled(format!("{number:>digits$}  "), number_style)],
            vec![pad(digits + 2)],
        )
    };
    let indent = first.iter().map(Span::width).sum();
    let code = shell_output::parse_sgr(&expand_tabs(&line.code), Tones::None).0;
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
/// `lead`. Lines a hunk skips read as a count across the diff. A preview
/// cuts each line at [`RESULT_LINE_ROWS`] rows a side and folds the rows
/// past [`DIFF_ROWS`] into a count, unless only one would, since a count
/// standing for one row is that row; expanded, every line is drawn whole
/// within [`EXPANDED_ROWS`] rows. Returns the rows and whether any line was
/// left out.
fn diff_view(
    output: &[String],
    lang: Option<Lang>,
    width: usize,
    look: Look,
    lead: impl Fn() -> Vec<Span<'static>>,
    detail: Detail,
) -> (Vec<Line<'static>>, bool) {
    let marks = look.marks();
    let full = detail == Detail::Full;
    let mut rows = diff::parse(output);
    let mut folded = 0;
    if !full {
        let kept = cards::shown(rows.len(), DIFF_ROWS);
        folded = rows.len() - kept;
        rows.truncate(kept);
    }
    let digits = diff::number_width(&rows);
    let lead_cells: usize = lead().iter().map(Span::width).sum();
    let room = width.saturating_sub(lead_cells);
    let count = |text: String| {
        let mut spans = lead();
        spans.push(pad(digits + 1));
        note(spans, &format!("{} {text}", marks.more), width)
    };
    let side = |line: Option<&DiffLine>, cells: usize, carry: &mut Carry| {
        let mut rows = diff_side(line, cells, digits, lang, carry, (look, true));
        if !full {
            clip_rows(&mut rows, RESULT_LINE_ROWS);
        }
        rows
    };
    let mut lines = Vec::new();
    if width >= SPLIT_MIN {
        let half = room.saturating_sub(DIVIDER_CELLS);
        let (left_cells, right_cells) = (half / 2, half - half / 2);
        let (mut old, mut new) = (Carry::default(), Carry::default());
        let split = diff::split(&rows);
        for (at, row) in split.iter().enumerate() {
            if full && lines.len() >= EXPANDED_ROWS {
                folded = split[at..]
                    .iter()
                    .filter(|row| matches!(row, SplitRow::Pair(..)))
                    .count();
                break;
            }
            match row {
                SplitRow::Skipped(n) => {
                    lines.extend(count(cards::count(*n, copy::UNMODIFIED_NOUNS)))
                }
                SplitRow::Pair(left, right) => {
                    let l = side(*left, left_cells, &mut old);
                    let r = side(*right, right_cells, &mut new);
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
                        spans.push(Span::styled(format!(" {} ", marks.gutter), dim()));
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
        for (at, row) in rows.iter().enumerate() {
            if full && lines.len() >= EXPANDED_ROWS {
                folded = rows[at..]
                    .iter()
                    .filter(|row| matches!(row, DiffRow::Line(_)))
                    .count();
                break;
            }
            match row {
                DiffRow::Skipped(n) => {
                    lines.extend(count(cards::count(*n, copy::UNMODIFIED_NOUNS)))
                }
                DiffRow::Line(line) => {
                    for side in side(Some(line), room, &mut carry) {
                        let mut spans = lead();
                        spans.extend(side.spans);
                        lines.push(Line::from(spans));
                    }
                }
            }
        }
    }
    let cut = full && lines.len() > EXPANDED_ROWS;
    if full {
        clip_rows(&mut lines, EXPANDED_ROWS);
    }
    if folded > 0 {
        lines.extend(count(cards::count(folded, copy::MORE_LINE_NOUNS)));
    }
    (lines, cut || folded > 0)
}

/// Lines of code, each numbered from `first` with `change`'s mark, if it
/// has one, and coloured as `lang`, unified at any width. Each line opens with `lead`;
/// a preview draws the first `limit` lines, each cut at
/// [`RESULT_LINE_ROWS`] rows, and expanded every line within
/// [`EXPANDED_ROWS`] rows; the rest folds into a count. Only the lines drawn
/// are read, so a large file costs no more than a small one. Returns the
/// rows and whether any line was left out.
#[allow(clippy::too_many_arguments)]
fn code_view(
    code: &[&str],
    change: Change,
    first: usize,
    lang: Option<Lang>,
    width: usize,
    look: Look,
    lead: impl Fn() -> Vec<Span<'static>>,
    (detail, limit): (Detail, usize),
) -> (Vec<Line<'static>>, bool) {
    let full = detail == Detail::Full;
    let kept = if full {
        code.len().min(EXPANDED_ROWS)
    } else {
        cards::shown(code.len(), limit)
    };
    let digits = (first + kept.max(1) - 1).to_string().len();
    // Context marks nothing, so its column is left out and the count sits
    // under the code; a change's count sits under its mark.
    let marked = change != Change::Context;
    let room = width.saturating_sub(lead().iter().map(Span::width).sum());
    let mut carry = Carry::default();
    let line = |index: usize, carry: &mut Carry| {
        let number = first + index;
        let drawn = DiffLine {
            change,
            old: Some(number),
            new: Some(number),
            code: code[index].to_owned(),
            emphasis: None,
        };
        let mut rows = diff_side(Some(&drawn), room, digits, lang, carry, (look, marked));
        if !full {
            clip_rows(&mut rows, RESULT_LINE_ROWS);
        }
        rows.into_iter()
            .map(|row| {
                let mut spans = lead();
                spans.extend(row.spans);
                Line::from(spans)
            })
            .collect::<Vec<_>>()
    };
    let more = |folded: usize| {
        let mut spans = lead();
        spans.push(pad(digits + if marked { 1 } else { 2 }));
        let text = format!(
            "{} {}",
            look.marks().more,
            cards::count(folded, copy::MORE_LINE_NOUNS)
        );
        note(spans, &text, width)
    };
    if full {
        return bounded(
            code.len(),
            EXPANDED_ROWS,
            |index| line(index, &mut carry),
            more,
        );
    }
    let mut lines: Vec<Line<'static>> = (0..kept)
        .flat_map(|index| line(index, &mut carry))
        .collect();
    let folded = code.len() - kept;
    if folded > 0 {
        lines.extend(more(folded));
    }
    (lines, folded > 0)
}

/// A write's content as added lines, numbered from 1 and coloured as
/// `lang`, since a new file has no old side to set beside it; a preview
/// draws its first [`WRITE_ROWS`] lines.
fn write_view(
    output: &[String],
    lang: Option<Lang>,
    width: usize,
    look: Look,
    lead: impl Fn() -> Vec<Span<'static>>,
    detail: Detail,
) -> (Vec<Line<'static>>, bool) {
    let code: Vec<&str> = output
        .iter()
        .map(|line| line.strip_prefix('+').unwrap_or(line))
        .collect();
    code_view(
        &code,
        Change::Added,
        1,
        lang,
        width,
        look,
        lead,
        (detail, WRITE_ROWS),
    )
}

/// The path a read's argument names and the line its range starts at:
/// `src/a.rs:40-60` reads from line 40, a bare path from line 1.
fn read_start(argument: &str) -> (&str, usize) {
    let Some((path, range)) = argument.rsplit_once(':') else {
        return (argument, 1);
    };
    let from = range.split_once('-').map_or(range, |(from, _)| from);
    match from.parse::<usize>() {
        Ok(line) if line > 0 && !path.is_empty() => (path, line),
        _ => (argument, 1),
    }
}

/// A read's content, expanded: numbered from the line the read started at
/// and coloured as its path's language, without blank lines at its end.
fn read_view(
    output: &[String],
    argument: &str,
    width: usize,
    look: Look,
    lead: impl Fn() -> Vec<Span<'static>>,
) -> (Vec<Line<'static>>, bool) {
    let (path, first) = read_start(argument);
    let end = output
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |i| i + 1);
    let code: Vec<&str> = output[..end].iter().map(String::as_str).collect();
    code_view(
        &code,
        Change::Context,
        first,
        syntax::lang_for_path(path),
        width,
        look,
        lead,
        (Detail::Full, EXPANDED_ROWS),
    )
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

/// A [`Row::Call`]'s parts, as [`present_call`] draws them.
struct Call<'a> {
    tool: &'a str,
    argument: &'a str,
    fields: &'a [(String, String)],
    id: Option<&'a str>,
    state: CallState,
    summary: Option<&'a str>,
    output: &'a [String],
    full_output_path: Option<&'a str>,
}

/// What a call's body drew.
struct Body {
    lines: Vec<Line<'static>>,
    /// Whether the body left any of the output out, or drew none of it, so
    /// the head says how much there is.
    left_out: bool,
    /// The output's lines that hold text, as the body reads them.
    count: usize,
}

/// A call as one block: `✓ Bash: argument` with its status after it, the
/// job or agent it reached tagged first, then its output hung from a
/// gutter in the shape its [`Kind`] gives it, then the file holding the
/// whole output when the tool cut it. A failed call's output is its
/// error, drawn as text whatever its kind.
fn present_call(call: Call, width: usize, look: Look, detail: Detail) -> Vec<Line<'static>> {
    let Call {
        tool,
        argument,
        id,
        state,
        summary,
        output,
        full_output_path,
        ..
    } = call;
    let failed = state == CallState::Failed;
    let quiet = if failed { red(look) } else { dim() };
    let (mut lines, under) = head(vec![pad(RAIL)], vec![pad(RAIL)], &call, width, look);
    let kind = kind(tool);
    let glyph = gutter(look);
    let gutter = || vec![pad(RAIL), Span::styled(format!("{glyph} "), quiet)];
    let full = detail == Detail::Full;
    let lang = || syntax::lang_for_path(argument);
    // A body drawn from the output's own shape, whose head counts lines
    // only when it leaves some out.
    let shaped = |(lines, left_out): (Vec<Line<'static>>, bool)| Body {
        lines,
        left_out,
        count: output.iter().filter(|line| !line.trim().is_empty()).count(),
    };
    let body = match kind {
        _ if failed => text_body(output, kind, argument, state, detail, width, look, &gutter),
        Kind::Edit => shaped(diff_view(output, lang(), width, look, gutter, detail)),
        Kind::Write => shaped(write_view(output, lang(), width, look, gutter, detail)),
        Kind::Grep => shaped(cards::matches(output, width, look, &gutter, detail)),
        Kind::List => shaped(cards::listing(output, width, look, &gutter, detail)),
        Kind::WebSearch => shaped(cards::results(output, width, look, &gutter, detail)),
        // A read's content is the file the model saw; its status says how
        // much, and only the expanded card draws it.
        Kind::Read if full => shaped(read_view(output, argument, width, look, gutter)),
        Kind::Read | Kind::Fetch if !full => shaped((
            Vec::new(),
            output.iter().any(|line| !line.trim().is_empty()),
        )),
        _ => text_body(output, kind, argument, state, detail, width, look, &gutter),
    };
    let diff = matches!(kind, Kind::Edit | Kind::Write) && !failed;
    // Without a summary of its own, an edit or a write says what it added
    // and removed, and any other call says how much output it has when its
    // body leaves some out.
    let mut status = match summary {
        Some(summary) => status_spans(summary, quiet, look),
        None if diff => diff_counts(output, look),
        None if body.left_out => line_count(body.count)
            .map(|count| vec![Span::styled(count, quiet)])
            .unwrap_or_default(),
        None => Vec::new(),
    };
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        let mut tagged = vec![tag(id, TagTone::Blue, look.tones)];
        if !status.is_empty() {
            tagged.push(pad(1));
        }
        tagged.append(&mut status);
        status = tagged;
    }
    ride(&mut lines, status, under, width);
    lines.extend(body.lines);
    if let Some(path) = full_output_path.filter(|path| !path.is_empty()) {
        lines.extend(hang(
            &format!("{} {}", copy::FULL_OUTPUT, clean_line(path)),
            width,
            BODY,
            gutter(),
            gutter(),
            dim(),
        ));
    }
    lines
}

/// `output` laid out as indented JSON when all of it is one JSON object or
/// array, so a tool's minified reply reads a member a line; `None` for any
/// other output. Output past [`json::MAX_BYTES`] is never joined.
fn json_lines(output: &[String]) -> Option<Vec<String>> {
    let first = output.iter().find(|line| !line.trim().is_empty())?;
    if !first.trim_start().starts_with(['{', '[']) {
        return None;
    }
    let bytes: usize = output.iter().map(|line| line.len() + 1).sum();
    if bytes > json::MAX_BYTES + 1 {
        return None;
    }
    json::pretty(&output.join("\n"))
}

/// A call's output drawn as text: a shell command's in its own colours or
/// the colours its shape implies, one JSON value laid out and coloured as
/// code, anything else plain, never with an escape. A preview draws the
/// lines [`plan`] picks for the call's state, each cut at
/// [`RESULT_LINE_ROWS`] rows; expanded, every line wraps in full within
/// [`EXPANDED_ROWS`] rows, a running call's newest.
#[allow(clippy::too_many_arguments)]
fn text_body(
    output: &[String],
    kind: Kind,
    argument: &str,
    state: CallState,
    detail: Detail,
    width: usize,
    look: Look,
    gutter: &dyn Fn() -> Vec<Span<'static>>,
) -> Body {
    let marks = look.marks();
    let json = json_lines(output);
    let shell = kind == Kind::Shell && json.is_none();
    let family = if shell {
        shell_output::family(argument)
    } else {
        Family::Other
    };
    let styled: Vec<Styled> = match &json {
        Some(lines) => lines
            .iter()
            .map(|line| Styled {
                text: shell_output::parse_sgr(line, Tones::None).0,
                runs: Vec::new(),
            })
            .collect(),
        None => output
            .iter()
            .map(|line| shell_output::styled(line, family, look.tones))
            .collect(),
    };
    let texts: Vec<String> = styled.iter().map(|line| line.text.clone()).collect();
    let count = texts.iter().filter(|line| !line.trim().is_empty()).count();
    let draw = |index: usize, cut: bool| {
        let text = &texts[index];
        // A JSON line is lexed alone: no token of it spans lines.
        let runs = if json.is_some() {
            code_runs(
                text,
                Some(Lang::TypeScript),
                &mut Carry::default(),
                look.tones,
            )
        } else if shell {
            styled[index].runs.clone()
        } else {
            Vec::new()
        };
        let mut rows = hang_runs(
            text,
            &runs,
            output_tone(look.tones),
            width,
            BODY,
            gutter(),
            gutter(),
        );
        if cut {
            clip_rows(&mut rows, RESULT_LINE_ROWS);
        }
        rows
    };
    let noted = |text: String| note(gutter(), &text, width);
    let more = |count: usize| {
        noted(format!(
            "{} {}",
            marks.more,
            cards::count(count, copy::MORE_LINE_NOUNS)
        ))
    };
    let above = |count: usize| noted(format!("+{count} {}", copy::ABOVE));
    let (start, end) = ends(&texts);
    let (lines, left_out) = match (detail, state) {
        (Detail::Preview, _) => {
            let policy = match state {
                CallState::Running => Policy::Newest,
                CallState::Failed => Policy::Failed,
                CallState::Done => Policy::Ends,
            };
            let mut lines = Vec::new();
            let mut left_out = false;
            for item in plan(&texts, policy) {
                match item {
                    Item::Line(index) => lines.extend(draw(index, true)),
                    Item::More(count) => {
                        left_out = true;
                        lines.extend(more(count));
                    }
                    Item::Above(count) => {
                        left_out = true;
                        lines.extend(above(count));
                    }
                }
            }
            (lines, left_out)
        }
        // Expanded, a running call follows its newest lines, as its preview
        // does.
        (Detail::Full, CallState::Running) => {
            let mut blocks = Vec::new();
            let mut rows = 0;
            let mut at = end;
            while at > start && rows < EXPANDED_ROWS {
                at -= 1;
                let block = draw(at, false);
                rows += block.len();
                blocks.push(block);
            }
            // The oldest line drawn may cross the bound: it is counted with
            // the lines above it, unless it is the only one.
            if rows > EXPANDED_ROWS && blocks.len() > 1 {
                blocks.pop();
                at += 1;
            }
            let mut newest: Vec<Line<'static>> = blocks.into_iter().rev().flatten().collect();
            let cut = newest.len() > EXPANDED_ROWS;
            clip_rows(&mut newest, EXPANDED_ROWS);
            let mut lines = Vec::new();
            if at > start {
                lines.extend(above(at - start));
            }
            lines.extend(newest);
            (lines, at > start || cut)
        }
        (Detail::Full, _) => bounded(
            end - start,
            EXPANDED_ROWS,
            |index| draw(start + index, false),
            more,
        ),
    };
    Body {
        lines,
        left_out,
        count,
    }
}

pub(crate) fn yellow(look: Look) -> Style {
    colour(look.tones, (0xfb, 0xbf, 0x24), Color::Yellow)
}

pub(crate) fn output_tone(tones: Tones) -> Style {
    colour(tones, (0xb4, 0xb8, 0xbf), Color::Gray)
}

/// One line of a result's preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    Line(String),
    /// The count of lines between the first and the last.
    More(usize),
    /// The count of lines before a running call's newest ones.
    Above(usize),
}

/// Which lines of an output a preview draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Policy {
    /// The first and last halves of [`RESULT_LINES`] around a count.
    Ends,
    /// A failed call's: its first [`FAILED_HEAD`] lines and the rest of
    /// [`FAILED_LINES`] from its end, around a count.
    Failed,
    /// A running call's newest [`RESULT_LINES`] under a count of the lines
    /// above them.
    Newest,
}

/// One row of a preview's plan, by the output line's index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Line(usize),
    More(usize),
    Above(usize),
}

/// The indices of `lines` past the blank lines at either end.
fn ends(lines: &[String]) -> (usize, usize) {
    let blank = |line: &String| line.trim().is_empty();
    let start = lines
        .iter()
        .position(|line| !blank(line))
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !blank(line))
        .map_or(start, |i| i + 1);
    (start, end)
}

/// The lines a preview draws under `policy`: every line when they are at
/// most its limit (or one more, since a count standing for one line is that
/// line), else its head and tail around a count of the lines between. Blank
/// lines at either end are left out, and a run of blank lines inside reads
/// as one.
fn plan(lines: &[String], policy: Policy) -> Vec<Item> {
    let (start, end) = ends(lines);
    let blank = |i: usize| lines[i].trim().is_empty();
    // `start` holds text, so `i - 1` is never read before it.
    let keep: Vec<usize> = (start..end)
        .filter(|&i| !blank(i) || !blank(i - 1))
        .collect();
    let (head, tail) = match policy {
        Policy::Ends => (RESULT_LINES / 2, RESULT_LINES - RESULT_LINES / 2),
        Policy::Failed => (FAILED_HEAD, FAILED_LINES - FAILED_HEAD),
        Policy::Newest => (0, RESULT_LINES),
    };
    if keep.len() <= head + tail + 1 {
        return keep.into_iter().map(Item::Line).collect();
    }
    let cut = keep.len() - tail;
    let mut items: Vec<Item> = keep[..head].iter().copied().map(Item::Line).collect();
    items.push(match head {
        0 => Item::Above(keep[cut] - start),
        _ => Item::More(keep[cut] - keep[head - 1] - 1),
    });
    items.extend(keep[cut..].iter().copied().map(Item::Line));
    items
}

/// `plan`'s items with the lines they name.
fn previewed(output: &[String], policy: Policy) -> Vec<Preview> {
    plan(output, policy)
        .into_iter()
        .map(|item| match item {
            Item::Line(index) => Preview::Line(output[index].clone()),
            Item::More(count) => Preview::More(count),
            Item::Above(count) => Preview::Above(count),
        })
        .collect()
}

/// The output a settled call draws: every line when there are at most
/// [`RESULT_LINES`] (or one more, since a count standing for one line is
/// that line), else the first half and the last half around a count. Blank
/// lines at either end are left out, and a run of them inside reads as one.
pub fn preview(output: &[String]) -> Vec<Preview> {
    previewed(output, Policy::Ends)
}

/// The output a running call draws: every line while there are at most
/// [`RESULT_LINES`] (or one more), then its newest [`RESULT_LINES`] under a
/// `+N above` count of the rest, so the preview follows the output at a
/// fixed height while the count grows, as the composer's window follows
/// its caret. Blank lines read as [`preview`] reads them.
pub fn live_preview(output: &[String]) -> Vec<Preview> {
    previewed(output, Policy::Newest)
}

/// The output a failed call draws: its first [`FAILED_HEAD`] lines and its
/// last lines, [`FAILED_LINES`] in all, around a count, since an error
/// usually ends the output. Blank lines read as [`preview`] reads them.
pub fn failed_preview(output: &[String]) -> Vec<Preview> {
    previewed(output, Policy::Failed)
}

/// The first line the viewport shows: a row and a line within it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchor {
    pub row: usize,
    pub line: usize,
}

/// The transcript's rows and the viewport over them. `anchor` is `None`
/// while following new output, which holds the newest line at the bottom.
/// The first `committed` rows are settled; any after them are live rows a
/// runtime is still writing, which its next update replaces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    pub rows: Vec<Row>,
    committed: usize,
    pub anchor: Option<Anchor>,
    /// The viewport's size on the last frame; paging needs both.
    pub width: usize,
    pub height: usize,
    /// How the last frame drew, so measuring matches drawing.
    pub look: Look,
    /// Each row's height at `measured`, for the scrollbar. Measured once per
    /// width and look, and again for rows appended or changed since.
    heights: Vec<usize>,
    /// Whether each measured row is a card with a count, and the newest
    /// such row, which Ctrl+O's hint follows; `None` until measured again
    /// after a change.
    counted: Vec<bool>,
    hint_row: Option<usize>,
    measured: Option<(usize, Tones, bool, Detail)>,
    /// Rows there were when output was last followed; rows past it arrived
    /// while reading.
    seen: usize,
    /// How much of each call's output the rows draw; in memory only, as
    /// Pi's expanded tools are.
    detail: Detail,
    hint: Hint,
}

/// Whether the count of the newest card that has one names Ctrl+O: never
/// until a view that can draw expanded history again offers it, then until
/// the key is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Hint {
    #[default]
    Off,
    Offered,
    Used,
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
            committed: rows.len(),
            rows,
            ..Self::default()
        }
    }

    /// Rows settled for good; the rest are live.
    pub fn committed(&self) -> usize {
        self.committed
    }

    /// Replaces the live rows with `rows`.
    pub fn set_live(&mut self, rows: Vec<Row>) {
        self.replace_live(rows);
    }

    /// Replaces the live rows with `rows` and settles them, so what was
    /// streamed is drawn once, in its settled form.
    pub fn commit(&mut self, rows: Vec<Row>) {
        self.replace_live(rows);
        self.committed = self.rows.len();
    }

    fn replace_live(&mut self, rows: Vec<Row>) {
        let from = self.committed;
        // An unchanged row keeps its measured height; only what changed,
        // and what follows it, is measured again.
        let same = self.rows[from..]
            .iter()
            .zip(&rows)
            .take_while(|(old, new)| old == new)
            .count();
        self.rows.truncate(from + same);
        self.rows.extend(rows.into_iter().skip(same));
        self.touched(from + same);
        // A reading position on a live row that went away falls back to the
        // newest row's start.
        if let Some(anchor) = self.anchor
            && anchor.row >= self.rows.len()
        {
            self.anchor = Some(Anchor {
                row: self.rows.len().saturating_sub(1),
                line: 0,
            });
            self.settle();
        }
    }

    fn lines(&self, index: usize) -> Vec<Line<'static>> {
        // Bake: the hint follows the newest card with a count, so a short
        // call after a folded one leaves it where it was.
        let hinted = self.hint == Hint::Offered && self.hint_row == Some(index);
        let (width, look) = (self.width, self.look);
        present_row(&self.rows, index, width, look, self.detail, hinted).0
    }

    /// Starts over with `rows`, as [`Transcript::new`] does, for another
    /// session or a compacted one; Ctrl+O's hint, once used, stays gone.
    pub fn replace(&mut self, rows: Vec<Row>) {
        *self = Self {
            hint: self.hint,
            ..Self::new(rows)
        };
    }

    /// Lets the count of the newest card that has one name Ctrl+O, as
    /// ` · Ctrl+O`, until the key is first used. Only the full-screen view offers it: inline, rows
    /// already in the scrollback are never drawn again expanded. The hint
    /// only fills room on the count's row, so no row's height changes.
    pub fn offer_expand_hint(&mut self) {
        if self.hint == Hint::Off {
            self.hint = Hint::Offered;
        }
    }

    /// How much of each call's output the rows draw.
    pub fn detail(&self) -> Detail {
        self.detail
    }

    /// Draws every call whole, or each call's preview again, keeping the
    /// reader's place: the row at the viewport's top stays there, at the
    /// nearest line it still has. Following output, it still follows.
    pub fn toggle_detail(&mut self) {
        self.detail = match self.detail {
            Detail::Preview => Detail::Full,
            Detail::Full => Detail::Preview,
        };
        self.hint = Hint::Used;
        self.measure();
        if let Some(anchor) = self.anchor {
            let count = self.count(anchor.row.min(self.rows.len().saturating_sub(1)));
            self.anchor = Some(Anchor {
                row: anchor.row,
                line: anchor.line.min(count.saturating_sub(1)),
            });
            self.settle();
        }
    }

    fn count(&self, index: usize) -> usize {
        self.lines(index).len()
    }

    /// Brings the row heights up to date: all of them after a width, look,
    /// or detail change, and otherwise only rows appended since.
    fn measure(&mut self) {
        let key = (self.width, self.look.tones, self.look.classic, self.detail);
        if self.measured != Some(key) {
            self.heights.clear();
            self.counted.clear();
            self.measured = Some(key);
        }
        self.heights.truncate(self.rows.len());
        self.counted.truncate(self.heights.len());
        for index in self.heights.len()..self.rows.len() {
            let (lines, counted) =
                present_row(&self.rows, index, self.width, self.look, self.detail, false);
            self.heights.push(lines.len());
            self.counted.push(counted);
        }
        self.hint_row = self.counted.iter().rposition(|&counted| counted);
    }

    /// Marks row `index` as changed, so its height is measured again.
    pub fn touched(&mut self, index: usize) {
        self.heights.truncate(index);
        self.counted.truncate(index);
        self.hint_row = None;
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
            Row::Codemode(script) => {
                script.state == CallState::Running
                    || script
                        .calls
                        .iter()
                        .any(|call| call.status == NestedStatus::Running)
            }
            _ => false,
        })
    }

    /// Whether the viewport is following new output.
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }

    /// Lines from `anchor` down, at most `limit`, each with the row and
    /// line that draw it.
    fn walk(&self, anchor: Anchor, limit: usize) -> Vec<(Anchor, Line<'static>)> {
        let mut out = Vec::new();
        let mut skip = anchor.line;
        for row in anchor.row..self.rows.len() {
            for (line, content) in self.lines(row).into_iter().enumerate().skip(skip) {
                if out.len() == limit {
                    return out;
                }
                out.push((Anchor { row, line }, content));
            }
            skip = 0;
        }
        out
    }

    fn lines_from(&self, anchor: Anchor, limit: usize) -> Vec<Line<'static>> {
        self.walk(anchor, limit)
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    }

    /// The lines the viewport shows, `height` of them at most.
    pub fn visible(&self) -> Vec<Line<'static>> {
        self.lines_from(self.top(), self.height)
    }

    /// The lines the viewport shows with the row and line that draw each,
    /// for a selection to find its text on screen.
    pub fn screen(&self) -> Vec<(Anchor, Line<'static>)> {
        self.screen_rows(self.height)
    }

    /// The lines `rows` screen rows show from the viewport's top, as
    /// [`Transcript::screen`] does at the viewport's height.
    ///
    /// Bake: a view may draw past that height without changing it, as the
    /// renderer draws on into the rows a finished turn's ending gives back
    /// once it has scrolled out of sight; scrolling still counts in the
    /// viewport's height.
    pub fn screen_rows(&self, rows: usize) -> Vec<(Anchor, Line<'static>)> {
        self.walk(self.top(), rows)
    }

    /// The text of the line at `at`, as drawn; empty past the transcript.
    pub fn line_text(&self, at: Anchor) -> String {
        self.lines(at.row)
            .get(at.line)
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    /// Every line from `start` to `end`, both included, as drawn: the text a
    /// selection copies.
    pub fn between(&self, start: Anchor, end: Anchor) -> Vec<(Anchor, String)> {
        let mut out = Vec::new();
        let mut skip = start.line;
        for row in start.row..=end.row.min(self.rows.len().saturating_sub(1)) {
            for (line, content) in self.lines(row).into_iter().enumerate().skip(skip) {
                let at = Anchor { row, line };
                if (row, line) > (end.row, end.line) {
                    return out;
                }
                out.push((at, content.to_string()));
            }
            skip = 0;
        }
        out
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

    // Bake: a running call's preview follows its newest output at a fixed
    // height under a `+N above` count that grows, as the composer's window
    // follows its caret; once it ends, the head-and-tail preview returns.
    #[test]
    fn a_running_call_follows_its_newest_output_under_a_growing_count() {
        let call = |state, count: usize| Row::Call {
            tool: "bash".into(),
            argument: "make".into(),
            fields: Vec::new(),
            id: None,
            state,
            summary: None,
            output: (1..=count).map(|n| format!("step {n}")).collect(),
            full_output_path: None,
        };
        let shown = |state, count| text(&present(&[call(state, count)], 0, 60, PLAIN));
        let few = shown(CallState::Running, 5);
        assert!(few.iter().any(|l| l.ends_with("step 1")), "{few:#?}");
        assert!(!few.iter().any(|l| l.contains(copy::ABOVE)), "{few:#?}");
        for count in [6, 40, 400] {
            let rows = shown(CallState::Running, count);
            let above = format!("+{} {}", count - RESULT_LINES, copy::ABOVE);
            let at = rows
                .iter()
                .position(|l| l.ends_with(&above))
                .unwrap_or_else(|| panic!("{above}: {rows:#?}"));
            let tail: Vec<String> = (count - RESULT_LINES + 1..=count)
                .map(|n| format!("step {n}"))
                .collect();
            let drawn: Vec<&str> = rows[at + 1..]
                .iter()
                .map(|l| l.trim_start_matches([' ', '│']))
                .collect();
            assert_eq!(drawn, tail, "{rows:#?}");
            assert_eq!(rows.len(), few.len(), "a fixed height: {rows:#?}");
        }
        let done = shown(CallState::Done, 40);
        assert!(done.iter().any(|l| l.ends_with("step 1")), "{done:#?}");
        assert!(
            done.iter().any(|l| l.contains(copy::MORE_LINES)),
            "{done:#?}"
        );
    }

    // Bake: a wrapped row stays inside its width when the spaces a wrap
    // leaves at its end are followed by a zero-width character, which
    // `trim_end` keeps (found by `markdown`'s width property).
    #[test]
    fn a_row_with_trailing_spaces_before_a_zero_width_character_is_clipped() {
        let text = format!("{}        \u{200b}", "a".repeat(20));
        for width in 10..40 {
            for line in hang_runs(
                &text,
                &[],
                Style::new(),
                width,
                2,
                vec![pad(2)],
                vec![pad(2)],
            ) {
                assert!(line.width() <= width, "{width}: {line:?}");
            }
        }
    }

    // Bake: a long shell command and one long output line stay a few rows,
    // the command counting the rows it folds and the line cut with `…`, so
    // a call's preview never fills the screen.
    #[test]
    fn a_long_command_and_a_long_output_line_are_bounded() {
        let script: String = (0..40).map(|n| format!("echo line {n}\n")).collect();
        let rows = vec![Row::Call {
            tool: "bash".into(),
            argument: format!("python3 - <<'EOF'\n{script}EOF"),
            fields: Vec::new(),
            id: None,
            state: CallState::Done,
            summary: None,
            output: vec!["x".repeat(5000), "tail".into()],
            full_output_path: None,
        }];
        let lines = text(&present(&rows, 0, 80, PLAIN));
        let more = lines
            .iter()
            .position(|line| line.contains(copy::LINES))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert_eq!(more, ARGUMENT_ROWS - 1, "{lines:#?}");
        // The count rides the last kept row, at its right edge.
        assert!(
            lines[more].starts_with("  ✓ ") || lines[more].contains("echo line"),
            "{lines:#?}"
        );
        assert!(lines[more].ends_with("+36 lines"), "{lines:#?}");
        let long: Vec<&String> = lines.iter().filter(|line| line.contains("xxxx")).collect();
        assert_eq!(long.len(), RESULT_LINE_ROWS, "{lines:#?}");
        assert!(
            long[RESULT_LINE_ROWS - 1].trim_end().ends_with('…'),
            "{lines:#?}"
        );
        assert!(lines.iter().all(|line| line.width() <= 80), "{lines:#?}");
        assert!(
            lines.last().is_some_and(|line| line.contains("tail")),
            "{lines:#?}"
        );
        assert!(
            lines.len() <= 1 + ARGUMENT_ROWS + RESULT_LINE_ROWS + 2,
            "{lines:#?}"
        );
    }

    #[test]
    fn live_rows_are_replaced_until_a_commit_settles_them() {
        let mut t = session(40, 6);
        let base = t.committed();
        assert_eq!(base, t.rows.len());
        t.set_live(vec![Row::Answer("a".into())]);
        t.set_live(vec![Row::Answer("a b".into()), Row::Reasoning("c".into())]);
        assert_eq!(t.rows.len(), base + 2);
        assert_eq!(t.committed(), base);
        t.commit(vec![Row::Answer("done".into())]);
        assert_eq!(t.rows[base..], [Row::Answer("done".into())]);
        assert_eq!(t.committed(), base + 1);
        // The height measured for the scrollbar follows the replacement.
        t.resize(40, 6, PLAIN);
        let lines: usize = (0..t.rows.len()).map(|i| t.count(i)).sum();
        assert_eq!(t.total(), lines);
        // A reading position on a live row that goes away holds on the
        // newest row rather than past the end.
        t.set_live(vec![Row::Answer("x".into()), Row::Answer("y".into())]);
        t.anchor = Some(Anchor {
            row: base + 2,
            line: 0,
        });
        t.set_live(Vec::new());
        assert!(t.anchor.is_none_or(|a| a.row < t.rows.len()));
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
        assert!(all[user - 1].starts_with("---"), "a rule opens the turn");
        assert_eq!(all[user - 2], "", "a blank ends the last turn");
        assert_eq!(all[user + 1], "");
        assert_eq!(
            all[user + 2],
            "  The registry is the list, so discovery should read it."
        );
        assert_eq!(all[user + 3], "");
        // The tool name has its own column; the status follows the argument.
        assert_eq!(
            all[user + 4],
            r#"  ✓ Bash: rg -n "commands.register" -g '*.ts'"#
        );
        assert!(
            all[user + 5] == "  │ packages/app/src/controller.ts:45:  commands.register(open);"
        );
        assert!(
            all[user + 6] == "  │ packages/app/src/controller.ts:52:  commands.register(close);"
        );
        // Each call stands apart from the next, so its box does too.
        assert_eq!(all[user + 7], "");
        // A read says the lines it asked for and how many it read, and
        // draws none of them.
        assert_eq!(
            all[user + 8],
            "  ✓ Read: packages/app/src/controller.ts:44-53  10 lines"
        );
        assert_eq!(all[user + 9], "");
        // A search's matches sit under their files, the rest counted.
        assert_eq!(
            all[user + 10..user + 19],
            [
                "  ✓ Grep: ctx.effect in packages/app  6 matches · 3 files",
                "  │ src/controller.ts",
                "  │   44  ctx.effect(() => {",
                "  │   51  ctx.effect(() => {",
                "  │ src/plugin.ts",
                "  │   27  const stop = ctx.effect(watch);",
                "  │   40  ctx.effect(() => stop());",
                "  │ ⋯ 2 more matches",
                "",
            ]
        );
        assert!(all[user + 19].starts_with("  Two registrations"));
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
        // A failed call keeps more of its end, where its error is.
        assert_eq!(
            lines[2..],
            [
                "  │ bun test v1.3.0",
                "  │ tests/parser.test.ts:",
                "  │ ⋯ 5 more lines",
                "  │ error: expect(received).toThrow()",
                "  │ Received function did not throw",
                "  │     at <anonymous> (tests/parser.test.ts:41:32)",
                "  │ (fail) rejects an unterminated quote",
                "  │  5 pass",
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
        assert!(all.iter().any(|l| l == "  | ... 5 more lines"));
    }

    #[test]
    fn prose_and_arguments_wrap_under_their_own_columns() {
        // Prose keeps the rail's two cells clear on the right as on the left.
        let rows = vec![Row::Answer("alpha beta gamma delta epsilon".into())];
        let drawn = text(&present(&rows, 0, 14, PLAIN));
        assert_eq!(drawn, ["  alpha beta", "  gamma", "  delta", "  epsilon"]);
        assert!(drawn.iter().all(|l| l.chars().count() <= 14 - RAIL));
        let user = vec![Row::User("alpha beta gamma".into())];
        assert_eq!(
            text(&present(&user, 0, 10, PLAIN)),
            ["> alpha", "  beta", "  gamma"]
        );
        let call = vec![Row::Call {
            tool: "Bash".into(),
            argument: "one two three four".into(),
            fields: Vec::new(),
            id: None,
            state: CallState::Running,
            summary: None,
            output: vec!["0123456789abcdef".into()],
            full_output_path: None,
        }];
        assert_eq!(
            text(&present(&call, 0, 18, PLAIN)),
            [
                "  ● Bash: one two",
                "          three",
                "          four",
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
        assert_eq!(text(&t.visible())[0], "  Bake");
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
            .position(|r| matches!(r, Row::Codemode(_)))
            .unwrap();
        text(&present(&rows, index, width, look))
    }

    #[test]
    fn only_a_codemode_head_is_marked_with_braces() {
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let lines = script_lines(80, look);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  {} codemode:  13 calls"))
        );
        // Its calls and every other call keep their state marks.
        assert!(lines.iter().any(|l| l.starts_with("    ✓ read")));
        let rows = sample_session();
        let all: Vec<String> = (0..rows.len())
            .flat_map(|i| text(&present(&rows, i, 80, look)))
            .collect();
        assert!(all.iter().any(|l| l.starts_with("  ✓ Bash: rg")));
        assert!(all.iter().all(|l| !l.contains("{} Bash")));
        let script = |state| {
            vec![Row::Codemode(Codemode {
                code: "return 1;".into(),
                state,
                summary: None,
                calls: Vec::new(),
                output: Vec::new(),
                full_output_path: None,
                wall_time: None,
            })]
        };
        let done = present(&script(CallState::Done), 0, 40, look);
        assert_eq!(fg_of(&done, "{}"), Some(Color::Rgb(0x22, 0xc5, 0x5e)));
        let failed = present(&script(CallState::Failed), 0, 40, look);
        assert_eq!(fg_of(&failed, "{}"), Some(Color::Rgb(0xef, 0x44, 0x44)));
        let on = present(&script(CallState::Running), 0, 40, look);
        assert_eq!(fg_of(&on, "{}"), Some(Color::Rgb(0xff, 0xff, 0xff)));
        // Without colour the braces cannot tell the states apart.
        assert!(script_lines(80, PLAIN)[1].starts_with("  ✓ codemode:"));
    }

    #[test]
    fn a_codemode_call_reads_as_its_script_then_its_calls_then_its_output() {
        assert_eq!(
            script_lines(80, PLAIN),
            [
                "",
                "  ✓ codemode:  13 calls · 1 failed · took 0.4s",
                // Code wraps under itself; eleven rows read whole, since a
                // count would stand for one.
                r#"  │  1  const files = (await tools.find({ pattern: "src/**/*.ts""#,
                r#"  │     })).split("\n");"#,
                "  │  2  const found = [];",
                "  │  3  for (const path of files) {",
                "  │  4    try {",
                r#"  │  5      if ((await tools.read({ path })).includes("TODO")) found.push(path);"#,
                "  │  6    } catch (error) {",
                "  │  7      console.log(`skipped ${path}: ${error.message}`);",
                "  │  8    }",
                "  │  9  }",
                "  │ 10  return found;",
                "  │",
                "  │ ⋯ 5 earlier calls",
                r#"  │ ✓ read  {"path":"src/m4.ts"}  3ms"#,
                r#"  │ ✗ read  {"path":"src/m5.ts"}  4ms"#,
                "  │   Permission denied",
                r#"  │ ✓ read  {"path":"src/m6.ts"}  2ms"#,
                r#"  │ ✓ read  {"path":"src/m7.ts"}  3ms"#,
                r#"  │ ✓ read  {"path":"src/m8.ts"}  4ms"#,
                r#"  │ ✓ read  {"path":"src/m9.ts"}  2ms"#,
                r#"  │ ✓ read  {"path":"src/m10.ts"}  3ms"#,
                r#"  │ ✓ read  {"path":"src/m11.ts"}  4ms"#,
                "  │",
                r#"  │ ["src/m3.ts","src/m9.ts"]"#,
                "  │ <console_output>",
                "  │ skipped src/m5.ts: Permission denied",
                "  │ </console_output>",
            ]
        );
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        let ascii = script_lines(80, classic);
        assert!(
            ascii.iter().all(|l| l.is_ascii() || l.contains('·')),
            "{ascii:#?}"
        );
        assert!(ascii.iter().any(|l| l == "  | ... 5 earlier calls"));
    }

    #[test]
    fn a_running_mark_blinks_white_in_place() {
        let rows = vec![Row::Call {
            tool: "Bash".into(),
            argument: "bun run build".into(),
            fields: Vec::new(),
            id: None,
            state: CallState::Running,
            summary: None,
            output: Vec::new(),
            full_output_path: None,
        }];
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let on = present(&rows, 0, 40, look);
        let off = present(&rows, 0, 40, Look { lit: false, ..look });
        // A call of one line is one tinted row, without padding rows.
        assert_eq!(text(&on), ["  ● Bash: bun run build"]);
        assert_eq!(text(&off), ["    Bash: bun run build"]);
        assert_eq!(on[0].width(), off[0].width());
        assert!(on[0].spans.iter().all(|s| s.style.bg.is_some()));
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

    #[test]
    fn a_status_too_long_for_its_row_wraps_under_the_argument() {
        let rows = vec![Row::Call {
            tool: "Bash".into(),
            argument: "make".into(),
            fields: Vec::new(),
            id: None,
            state: CallState::Failed,
            summary: Some("error: no rule to make target build".into()),
            output: Vec::new(),
            full_output_path: None,
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
    fn the_job_or_agent_a_call_reached_is_tagged_before_its_status() {
        let call = |tool: &str, argument: &str, summary: Option<&str>| Row::Call {
            tool: tool.into(),
            argument: argument.into(),
            fields: Vec::new(),
            id: Some(if tool == "Bash" { "bash-1" } else { "a1" }.into()),
            state: CallState::Done,
            summary: summary.map(str::to_owned),
            output: Vec::new(),
            full_output_path: None,
        };
        let rows = vec![
            call("Bash", "sleep 30", Some("background")),
            call("Agent", "review", None),
            call("Bash", "sleep 30", Some("already finished · exit 0")),
        ];
        let lines = |index, width| text(&present(&rows, index, width, PLAIN));
        assert_eq!(lines(0, 60), ["  ✓ Bash: sleep 30   bash-1  background"]);
        // Without a status, the tag stands alone.
        assert_eq!(lines(1, 60), ["", "  ✓ Agent: review   a1"]);
        assert_eq!(
            lines(2, 60),
            ["", "  ✓ Bash: sleep 30   bash-1  already finished · exit 0"]
        );
        // Too narrow for the tags, the status still reads, as text.
        assert_eq!(
            lines(2, 30),
            [
                "",
                "  ✓ Bash: sleep 30",
                "          bash-1  already",
                "          finished · exit 0"
            ]
        );
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let tagged = present(&rows, 0, 60, look);
        let id = tagged
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == " bash-1 ")
            .unwrap();
        assert_eq!(id.style.bg, Some(Color::Rgb(0x1c, 0x2b, 0x44)));
    }

    #[test]
    fn tools_are_sorted_into_kinds_by_name() {
        let kinds = [
            "Bash",
            "Read",
            "Edit",
            "Write",
            "Grep",
            "Find",
            "Ls",
            "Fetch",
            "Web fetch",
            "Web search",
            "Agent",
            "mcp_custom",
            "github · create_issue",
        ]
        .map(kind);
        assert_eq!(
            kinds,
            [
                Kind::Shell,
                Kind::Read,
                Kind::Edit,
                Kind::Write,
                Kind::Grep,
                Kind::List,
                Kind::List,
                Kind::Fetch,
                Kind::Fetch,
                Kind::WebSearch,
                Kind::Agent,
                Kind::Other,
                Kind::Other,
            ]
        );
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
            fields: Vec::new(),
            id: None,
            state: CallState::Done,
            summary: None,
            output: vec!["0123456789abcdefghij".into()],
            full_output_path: None,
        }];
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let lines = present(&rows, 0, 20, look);
        // Text wraps two cells short of the edge; the box still fills the
        // row.
        assert_eq!(
            text(&lines),
            ["", "  ✓ Bash: x", "    0123456789abcd", "    efghij", ""]
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
        // The script's source.
        assert_eq!(fg_of(&all, "await"), violet);
        assert_eq!(fg_of(&all, "find"), sky);
        assert_eq!(fg_of(&all, r#""TODO""#), lime);
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
            fields: Vec::new(),
            id: None,
            state: CallState::Done,
            summary: None,
            output: Vec::new(),
            full_output_path: None,
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
    fn a_dashed_rule_opens_each_turn_after_the_first_row() {
        let rows = sample_session();
        let all: Vec<String> = (0..rows.len())
            .flat_map(|i| text(&present(&rows, i, 40, PLAIN)))
            .collect();
        let user = all
            .iter()
            .position(|l| l.starts_with("> Find where"))
            .unwrap();
        assert_eq!(all[user - 2], "");
        assert_eq!(all[user - 1], "-".repeat(40));
        assert_eq!(
            all.iter().filter(|l| *l == &"-".repeat(40)).count(),
            3,
            "one per turn"
        );
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        assert_eq!(rule(10, RAIL, classic).to_string(), "  ------");
        assert_eq!(rule(6, 0, classic).to_string(), "------");
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
        assert_eq!(bg(one("stopped")), Some(Color::Rgb(0x3a, 0x34, 0x16)));
        assert_eq!(
            bg(one("outcome unknown")),
            Some(Color::Rgb(0x3a, 0x34, 0x16))
        );
        // Anything else stays text, one run even with several parts.
        assert_eq!(one("412 lines")[0].content, "412 lines");
        assert_eq!(bg(one("exit code")), None);
        assert_eq!(one("done · earlier process").len(), 1);
        // Each part of a status reads alone, and a tag's padding stands in
        // for the separator's space beside it.
        let parts = one("already finished · exit 1 · earlier process");
        let contents: Vec<&str> = parts.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            contents,
            [
                "already finished",
                " ·",
                " exit 1 ",
                "· ",
                "earlier process"
            ]
        );
        assert_eq!(parts[2].style.bg, Some(Color::Rgb(0x3f, 0x1d, 0x22)));
        assert!(
            parts
                .iter()
                .enumerate()
                .all(|(i, s)| i == 2 || s.style == quiet)
        );
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

    fn banner_rows() -> Vec<Row> {
        let pill = |text: &str, tone| Pill {
            text: text.into(),
            tone,
        };
        vec![
            Row::Banner(Banner {
                version: "0.4.2".into(),
                directory: "~/src/app".into(),
                session: "resumed · 6 prompts · 2h ago".into(),
                access: vec![
                    pill("workspace-write", TagTone::Blue),
                    pill("approvals ask", TagTone::Yellow),
                    pill("offline", TagTone::Pink),
                ],
                tip: None,
            }),
            Row::Notice(Notice {
                tag: "update".into(),
                text: "Bake 0.4.3 is available. Run bake update to install it.".into(),
            }),
        ]
    }

    #[test]
    fn the_banner_names_bake_its_version_session_and_access() {
        let rows = banner_rows();
        assert_eq!(
            text(&present(&rows, 0, 80, PLAIN)),
            [
                "",
                "  Bake v0.4.2 · code, freshly baked",
                "  directory  ~/src/app",
                "  session    resumed · 6 prompts · 2h ago",
                "  access      workspace-write   approvals ask   offline",
            ]
        );
        // Narrow, the tagline goes, a long value hangs under its column, and
        // the tags flow onto the next row instead of being cut.
        assert_eq!(
            text(&present(&rows, 0, 34, PLAIN)),
            [
                "",
                "  Bake v0.4.2",
                "  directory  ~/src/app",
                "  session    resumed · 6 prompts",
                "             · 2h ago",
                "  access      workspace-write",
                "              approvals ask",
                "              offline",
            ]
        );
        // Every row stays inside the width, however narrow.
        for width in 1..=90 {
            for line in present(&rows, 0, width, PLAIN) {
                assert!(line.width() <= width.max(RAIL + 12), "{width}: {line:?}");
            }
        }
        // An empty access list leaves its line out.
        let Row::Banner(mut banner) = rows[0].clone() else {
            unreachable!()
        };
        banner.access.clear();
        assert_eq!(present(&[Row::Banner(banner)], 0, 80, PLAIN).len(), 4);
    }

    // Bake: a compaction is a ruled mark with the context before and after
    // in compact counts, and its notes under it; a head too wide for its
    // rule wraps, and nothing escapes the width or reaches the terminal.
    #[test]
    fn a_compaction_mark_rules_off_its_counts_and_notes() {
        let mark = Row::Compacted(Compacted {
            title: "Auto-compacted".into(),
            before: 182_340,
            after: Some(21_050),
            notes: vec![
                "The summary read 99% of its 183,000 prompt tokens from the cache.".into(),
                "bad \x1b]0;title\x07 text".into(),
            ],
        });
        let rows = [Row::Answer("done".into()), mark.clone()];
        let drawn = text(&present(&rows, 1, 60, PLAIN));
        assert_eq!(
            drawn,
            [
                "",
                "  Auto-compacted · 182.3k → 21.1k tokens -----------------",
                "  The summary read 99% of its 183,000 prompt tokens from",
                "  the cache.",
                "  bad  text",
            ]
        );
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        assert!(text(&present(&rows, 1, 60, classic))[1].contains("182.3k -> 21.1k tokens"));
        let unmeasured = Row::Compacted(Compacted {
            title: "Compacted".into(),
            before: 950,
            after: None,
            notes: Vec::new(),
        });
        assert_eq!(
            text(&present(&[unmeasured], 0, 30, PLAIN)),
            ["  Compacted · 950 tokens ---"]
        );
        assert_eq!(
            text(&present(std::slice::from_ref(&mark), 0, 20, PLAIN))[..2],
            ["  Auto-compacted ·", "  182.3k → 21.1k"]
        );
        for width in 1..=90 {
            for line in present(&rows, 1, width, PLAIN) {
                assert!(line.width() <= width.max(RAIL + 2), "{width}: {line:?}");
            }
        }
        // The title takes the compaction blue.
        let blue = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let head = &present(&[mark], 0, 60, blue)[0];
        assert_eq!(
            head.spans[1].style.fg,
            Tones::TrueColor
                .style(crate::activity::Hue::Compacting, 0.0)
                .fg
        );
    }

    #[test]
    fn a_tip_hangs_dim_under_the_access_tags() {
        let mut rows = banner_rows();
        let Row::Banner(banner) = &mut rows[0] else {
            unreachable!()
        };
        banner.tip = Some("Shift+Tab cycles the thinking level".into());
        let drawn = present(&rows, 0, 80, PLAIN);
        assert_eq!(
            text(&drawn).last().map(String::as_str),
            Some("  tip        Shift+Tab cycles the thinking level")
        );
        let tip = drawn.last().unwrap();
        assert!(tip.spans.iter().skip(1).all(|span| span.style == dim()));
        // Narrow, it hangs under its column.
        assert_eq!(
            text(&present(&rows, 0, 34, PLAIN))[8..],
            [
                "  tip        Shift+Tab cycles",
                "             the thinking level",
            ]
        );
    }

    #[test]
    fn the_wordmark_glows_on_truecolor_and_falls_back_elsewhere() {
        let rows = banner_rows();
        let title = |look| present(&rows, 0, 80, look)[1].clone();
        let truecolor = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let letters: Vec<Style> = title(truecolor).spans[1..5]
            .iter()
            .map(|s| s.style)
            .collect();
        let rgb = |r, g, b| {
            Style::new()
                .fg(Color::Rgb(r, g, b))
                .add_modifier(Modifier::BOLD)
        };
        assert_eq!(
            letters,
            [
                rgb(0xf5, 0x9e, 0x0b),
                rgb(0xf7, 0x82, 0x12),
                rgb(0xf8, 0x62, 0x2e),
                rgb(0xf4, 0x3f, 0x5e),
            ]
        );
        // Sixteen colours and the classic frame take the accent; `NO_COLOR`
        // only bold. The text is the same in every case.
        let ansi = Look {
            tones: Tones::Ansi,
            ..PLAIN
        };
        let classic = Look {
            classic: true,
            ..truecolor
        };
        for (look, style) in [
            (ansi, accent(Tones::Ansi).add_modifier(Modifier::BOLD)),
            (
                classic,
                accent(Tones::TrueColor).add_modifier(Modifier::BOLD),
            ),
            (PLAIN, Style::new().add_modifier(Modifier::BOLD)),
        ] {
            let line = title(look);
            assert_eq!(line.spans[1].content, "Bake");
            assert_eq!(line.spans[1].style, style);
        }
        assert_eq!(text(&[title(truecolor)]), text(&[title(PLAIN)]));
    }

    #[test]
    fn a_notice_opens_with_its_tag_and_hangs_under_its_text() {
        let rows = banner_rows();
        assert_eq!(
            text(&present(&rows, 1, 40, PLAIN)),
            [
                "",
                "   update  Bake 0.4.3 is available.",
                "           Run bake update to install",
                "           it.",
            ]
        );
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let drawn = present(&rows, 1, 40, look);
        assert_eq!(
            drawn[1].spans[1],
            tag("update", TagTone::Yellow, Tones::TrueColor)
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
        let full = format!("  Bake  {strip}");
        assert_eq!(title(80, look), full);
        assert_eq!(title(full.width(), look), full);
        // Too narrow, without colour, or with the classic frame: the title alone.
        assert_eq!(title(full.width() - 1, look), "  Bake");
        assert_eq!(title(80, PLAIN), "  Bake");
        assert_eq!(
            title(
                80,
                Look {
                    classic: true,
                    ..look
                }
            ),
            "  Bake"
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
        let mut output = vec!["@@ -0,0 +1,30 @@".to_owned()];
        output.extend((0..30).map(|i| format!("+line {i}")));
        let call = |tool: &str, output: Vec<String>| Row::Call {
            tool: tool.into(),
            argument: "notes.txt".into(),
            fields: Vec::new(),
            id: None,
            state: CallState::Done,
            summary: None,
            output,
            full_output_path: None,
        };
        let lines = text(&present(&[call("Edit", output.clone())], 0, 80, PLAIN));
        assert_eq!(lines[0], "  ✓ Edit: notes.txt  +30");
        assert_eq!(lines.len(), 1 + DIFF_ROWS + 1, "{lines:#?}");
        assert_eq!(lines.last().unwrap(), "  │    ⋯ 14 more lines");
        assert_eq!(lines[1], "  │  1 + line 0");
        assert_eq!(lines[DIFF_ROWS], "  │ 16 + line 15");
        // A written file is new, so it folds sooner, and at any width it
        // reads as one column: there is no old side to set beside it.
        let written: Vec<String> = output[1..].to_vec();
        for width in [80, 140] {
            let lines = text(&present(&[call("Write", written.clone())], 0, width, PLAIN));
            assert_eq!(lines[0], "  ✓ Write: notes.txt  +30");
            assert_eq!(lines.len(), 1 + WRITE_ROWS + 1, "{lines:#?}");
            assert_eq!(lines[1], "  │ 1 + line 0");
            assert_eq!(lines[WRITE_ROWS], "  │ 6 + line 5");
            assert_eq!(lines.last().unwrap(), "  │   ⋯ 24 more lines");
        }
        // A count standing for one line is that line.
        let lines = text(&present(
            &[call("Write", written[..7].to_vec())],
            0,
            80,
            PLAIN,
        ));
        assert_eq!(lines.len(), 1 + 7);
    }

    #[test]
    fn a_failed_edit_or_write_shows_its_error_as_text() {
        for tool in ["Edit", "Write"] {
            let rows = vec![Row::Call {
                tool: tool.into(),
                argument: "src/x.rs".into(),
                fields: Vec::new(),
                id: None,
                state: CallState::Failed,
                summary: None,
                output: vec!["Could not find the exact text in src/x.rs.".into()],
                full_output_path: None,
            }];
            assert_eq!(
                text(&present(&rows, 0, 80, PLAIN)),
                [
                    format!("  ✗ {tool}: src/x.rs"),
                    "  │ Could not find the exact text in src/x.rs.".into(),
                ]
            );
        }
    }

    #[test]
    fn a_call_without_a_lead_argument_reads_as_its_fields() {
        let call = |fields: Vec<(&str, &str)>| {
            vec![Row::Call {
                tool: "github · create_issue".into(),
                argument: String::new(),
                fields: fields
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect(),
                id: None,
                state: CallState::Done,
                summary: Some("created".into()),
                output: Vec::new(),
                full_output_path: None,
            }]
        };
        let rows = call(vec![("repo", "acme/app"), ("title", "Fix it")]);
        let lines = present(&rows, 0, 80, PLAIN);
        assert_eq!(
            text(&lines),
            ["  ✓ github · create_issue: repo acme/app · title Fix it  created"]
        );
        // Keys and the marks between pairs are dim; values are not.
        let style = |content: &str| {
            lines[0]
                .spans
                .iter()
                .find(|span| span.content == content)
                .map(|span| span.style)
        };
        assert_eq!(style("repo"), Some(dim()));
        assert_eq!(style(" · "), Some(dim()));
        assert_ne!(style("acme/app"), Some(dim()));
        // A long head wraps under its argument, and nothing escapes.
        let rows = call(vec![
            ("body", "line one\u{1b}[31m red\u{7}"),
            ("labels", r#"["bug","ui"]"#),
        ]);
        let lines = text(&present(&rows, 0, 40, PLAIN));
        assert_eq!(
            lines,
            [
                "  ✓ github · create_issue: body line one",
                "                           red · labels",
                r#"                           ["bug","ui"]"#,
                "                           created",
            ]
        );
    }

    #[test]
    fn every_card_fits_its_width_in_every_look() {
        let rows = sample_session();
        let looks = [
            PLAIN,
            Look {
                classic: true,
                ..PLAIN
            },
            Look {
                tones: Tones::TrueColor,
                ..PLAIN
            },
        ];
        for width in [40, 60, 80, 100, 120, 160] {
            for look in looks {
                for index in 0..rows.len() {
                    for line in present(&rows, index, width, look) {
                        assert!(line.width() <= width, "{width} {look:?}: {line:?}");
                        let line = line.to_string();
                        assert!(!line.contains(char::is_control), "{line:?}");
                        if look.classic {
                            assert!(line.is_ascii() || line.contains('·'), "{line:?}");
                        }
                    }
                }
            }
        }
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
            fields: Vec::new(),
            id: None,
            state: CallState::Done,
            summary: None,
            output: vec!["\u{1b}[31mred\u{1b}[0m\u{1b}[2J\tend".into()],
            full_output_path: None,
        };
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        for (tool, coloured) in [("Bash", true), ("Notes", false)] {
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

    /// A call of `tool` with `output`, settled as `state`.
    fn card(tool: &str, argument: &str, state: CallState, output: Vec<String>) -> Vec<Row> {
        vec![Row::Call {
            tool: tool.into(),
            argument: argument.into(),
            fields: Vec::new(),
            id: None,
            state,
            summary: None,
            output,
            full_output_path: None,
        }]
    }

    fn numbered(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("l{i}")).collect()
    }

    fn drawn(rows: &[Row], width: usize, detail: Detail) -> Vec<String> {
        text(&present_detail(rows, 0, width, PLAIN, detail))
    }

    #[test]
    fn the_head_counts_lines_only_when_the_body_leaves_some_out() {
        let short = card("Bash", "ls", CallState::Done, numbered(3));
        assert_eq!(drawn(&short, 60, Detail::Preview)[0], "  ✓ Bash: ls");
        let long = card("Bash", "ls", CallState::Done, numbered(9));
        assert_eq!(
            drawn(&long, 60, Detail::Preview)[0],
            "  ✓ Bash: ls  9 lines"
        );
        // Expanded, nothing is left out, so nothing is counted.
        assert_eq!(drawn(&long, 60, Detail::Full)[0], "  ✓ Bash: ls");
        assert_eq!(drawn(&long, 60, Detail::Full).len(), 1 + 9);
        // A body that draws none of its output counts it.
        let fetch = card("Fetch", "https://a.test", CallState::Done, numbered(2));
        assert_eq!(
            drawn(&fetch, 60, Detail::Preview),
            ["  ✓ Fetch: https://a.test  2 lines"]
        );
    }

    #[test]
    fn a_failed_preview_keeps_its_end_and_blank_runs_read_as_one() {
        let failed = failed_preview(&numbered(20));
        assert_eq!(failed.len(), FAILED_LINES + 1);
        assert_eq!(
            failed[..2],
            [Preview::Line("l0".into()), Preview::Line("l1".into())]
        );
        assert_eq!(failed[2], Preview::More(20 - FAILED_LINES));
        assert_eq!(failed.last(), Some(&Preview::Line("l19".into())));
        assert_eq!(
            failed_preview(&numbered(FAILED_LINES + 1)).len(),
            FAILED_LINES + 1
        );
        // A run of blank lines inside reads as one blank row, and the count
        // still names every line it stands for.
        let gappy: Vec<String> = ["a", "", "", "", "b", " ", "c"].map(str::to_owned).to_vec();
        assert_eq!(
            preview(&gappy),
            ["a", "", "b", " ", "c"]
                .map(|line| Preview::Line(line.into()))
                .to_vec()
        );
        let mut spread = numbered(2);
        spread.extend(["", "", ""].map(str::to_owned));
        spread.extend(numbered(4));
        assert_eq!(preview(&spread)[2], Preview::More(5));
        let rows = card("Bash", "x", CallState::Done, gappy);
        assert_eq!(
            drawn(&rows, 40, Detail::Preview)[1..],
            ["  │ a", "  │", "  │ b", "  │", "  │ c"]
        );
        // Expanded, every blank line stays.
        assert_eq!(drawn(&rows, 40, Detail::Full).len(), 1 + 7);
    }

    #[test]
    fn json_output_is_laid_out_and_coloured_as_code() {
        let rows = sample_session();
        let index = rows
            .iter()
            .position(|row| matches!(row, Row::Call { tool, .. } if tool.contains("search_issues")))
            .unwrap();
        let lines = text(&present(&rows, index, 80, PLAIN));
        assert_eq!(
            lines[2..],
            [
                "  │ {",
                r#"  │   "number": 214,"#,
                r#"  │   "title": "Resolve the TODO in src/m3.ts","#,
                r#"  │   "state": "open""#,
                "  │ }",
            ]
        );
        // The head counts nothing: the whole value shows.
        assert!(lines[1].ends_with("state open"), "{lines:#?}");
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let coloured = present(&rows, index, 80, look);
        let key = fg_of(&coloured, r#""number""#);
        let number = fg_of(&coloured, "214");
        assert!(key.is_some() && number.is_some() && key != number);
        // Text that is not one JSON value is drawn as it came.
        let rows = card(
            "Notes",
            "x",
            CallState::Done,
            vec!["[1, 2] and more".into()],
        );
        assert_eq!(drawn(&rows, 40, Detail::Preview)[1], "  │ [1, 2] and more");
        // A long value is previewed like any other output.
        let wide = format!(
            "[{}]",
            (0..30).map(|n| n.to_string()).collect::<Vec<_>>().join(",")
        );
        let rows = card("Notes", "x", CallState::Done, vec![wide]);
        let lines = drawn(&rows, 40, Detail::Preview);
        assert_eq!(lines[0], "  ✓ Notes: x  32 lines");
        assert_eq!(lines[3], "  │ ⋯ 28 more lines");
        assert_eq!(drawn(&rows, 40, Detail::Full).len(), 1 + 32);
    }

    #[test]
    fn the_head_never_carries_an_escape() {
        let rows = vec![Row::Call {
            tool: "Ba\u{1b}[31msh\u{7}".into(),
            argument: "echo \u{1b}]0;title\u{7}one\ncat\u{1b}[2J two".into(),
            fields: Vec::new(),
            id: None,
            state: CallState::Done,
            summary: None,
            output: Vec::new(),
            full_output_path: None,
        }];
        let lines = present(&rows, 0, 60, PLAIN);
        // The command keeps its line break; the escapes are gone.
        assert_eq!(text(&lines), ["  ✓ Bash: echo one", "          cat two"]);
        for line in &lines {
            assert!(!line.to_string().contains(char::is_control), "{line:?}");
        }
        let path = card("Read", "a\u{1b}[1m/b.rs", CallState::Done, Vec::new());
        assert_eq!(drawn(&path, 60, Detail::Preview), ["  ✓ Read: a/b.rs"]);
    }

    #[test]
    fn a_long_diff_line_is_cut_in_the_preview_and_whole_expanded() {
        let long = "x".repeat(400);
        let output = vec![
            "@@ -1,1 +1,1 @@".to_owned(),
            format!("-{long}"),
            format!("+{long}y"),
        ];
        let rows = card("Edit", "a.rs", CallState::Done, output);
        for width in [80, 140] {
            let preview = drawn(&rows, width, Detail::Preview);
            // Unified, each side's line takes its own rows; split, they
            // share them.
            let bound = if width >= SPLIT_MIN { 1 } else { 2 } * RESULT_LINE_ROWS;
            assert_eq!(preview.len(), 1 + bound, "{width}: {preview:#?}");
            assert!(
                preview.iter().any(|line| line.contains('…')),
                "{preview:#?}"
            );
            let full = drawn(&rows, width, Detail::Full);
            assert!(full.len() > 1 + bound, "{width}: {full:#?}");
            assert!(!full.iter().any(|line| line.contains('…')), "{full:#?}");
        }
        // Expanded, a whole diff shows, without a fold.
        let mut big = vec!["@@ -1,40 +1,40 @@".to_owned()];
        big.extend((0..40).map(|i| format!("+line {i}")));
        let rows = card("Edit", "a.rs", CallState::Done, big);
        let full = drawn(&rows, 80, Detail::Full);
        assert_eq!(full.len(), 1 + 40);
        assert!(full.last().unwrap().contains("line 39"));
    }

    #[test]
    fn a_read_expanded_shows_its_lines_numbered_from_where_it_started() {
        let rows = sample_session();
        let index = rows
            .iter()
            .position(|row| matches!(row, Row::Call { tool, .. } if tool == "Read"))
            .unwrap();
        let preview = text(&present_detail(&rows, index, 80, PLAIN, Detail::Preview));
        assert_eq!(preview.len(), 2, "{preview:#?}");
        let full = text(&present_detail(&rows, index, 80, PLAIN, Detail::Full));
        // Two cells between the number and the code, as a script's source
        // reads, since a read has no change to mark.
        assert_eq!(full[2], "  │ 44    ctx.effect(() => {");
        assert_eq!(full[3], "  │ 45      commands.register(open);");
        assert_eq!(
            full.last().unwrap(),
            "  │ 53      return () => commands.unregister(close);"
        );
        assert_eq!(full.len(), 2 + 10);
        // A long line wraps to the code's column, past its number.
        let narrow = text(&present_detail(&rows, index, 36, PLAIN, Detail::Full));
        let end = narrow.iter().rposition(|row| row.contains("53")).unwrap();
        assert_eq!(narrow[end], "  │ 53      return () =>");
        assert_eq!(narrow[end + 1], "  │     commands.unregister(close);");
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let coloured = present_detail(&rows, index, 80, look, Detail::Full);
        // Coloured as the path's language: a keyword is not plain output.
        assert_ne!(fg_of(&coloured, "return"), output_tone(look.tones).fg);
        assert_eq!(read_start("src/a.rs:40-60"), ("src/a.rs", 40));
        assert_eq!(read_start("src/a.rs:7"), ("src/a.rs", 7));
        assert_eq!(read_start("src/a.rs"), ("src/a.rs", 1));
        assert_eq!(read_start("c:\\x"), ("c:\\x", 1));
    }

    #[test]
    fn a_saved_output_is_named_under_the_card() {
        let mut rows = card("Bash", "make", CallState::Done, numbered(2));
        if let Row::Call {
            full_output_path, ..
        } = &mut rows[0]
        {
            *full_output_path = Some("/tmp/bake-1.log\u{1b}[2J".into());
        }
        let lines = present(&rows, 0, 60, PLAIN);
        let last = lines.last().unwrap();
        assert_eq!(last.to_string(), "  │ Full output: /tmp/bake-1.log");
        assert_eq!(last.spans.last().unwrap().style, dim());
        let full = present_detail(&rows, 0, 60, PLAIN, Detail::Full);
        assert_eq!(full.last(), Some(last));
    }

    #[test]
    fn expanded_cards_stop_at_their_bound_and_count_the_rest() {
        let rows = card("Bash", "seq", CallState::Done, numbered(2000));
        let lines = drawn(&rows, 60, Detail::Full);
        assert_eq!(lines.len(), 1 + EXPANDED_ROWS + 1);
        assert_eq!(lines[0], "  ✓ Bash: seq  2000 lines");
        assert_eq!(lines[1], "  │ l0");
        assert_eq!(
            lines.last().unwrap(),
            &format!("  │ ⋯ {} more lines", 2000 - EXPANDED_ROWS)
        );
        // One left out is counted as one.
        let rows = card("Bash", "seq", CallState::Done, numbered(EXPANDED_ROWS + 1));
        let lines = drawn(&rows, 60, Detail::Full);
        assert_eq!(lines.last().unwrap(), "  │ ⋯ 1 more line");
        // A running call follows its newest lines.
        let rows = card("Bash", "seq", CallState::Running, numbered(2000));
        let lines = drawn(&rows, 60, Detail::Full);
        assert_eq!(lines.len(), 1 + 1 + EXPANDED_ROWS);
        assert_eq!(lines[1], format!("  │ +{} above", 2000 - EXPANDED_ROWS));
        assert_eq!(lines.last().unwrap(), "  │ l1999");
        // One line longer than the bound is cut at it.
        let rows = card(
            "Bash",
            "x",
            CallState::Done,
            vec!["y".repeat(60 * EXPANDED_ROWS)],
        );
        let lines = drawn(&rows, 60, Detail::Full);
        assert_eq!(lines.len(), 1 + EXPANDED_ROWS);
        assert!(lines.last().unwrap().ends_with('…'));
        // A write as large stops there too.
        let written: Vec<String> = (0..1000).map(|i| format!("+{i}")).collect();
        let rows = card("Write", "a.txt", CallState::Done, written);
        assert_eq!(drawn(&rows, 60, Detail::Full).len(), 1 + EXPANDED_ROWS + 1);
    }

    #[test]
    fn toggling_the_detail_keeps_the_place_being_read() {
        let mut t = session(80, 10);
        let before = t.total();
        // Following, it still follows, and every card grows.
        t.toggle_detail();
        assert_eq!(t.detail(), Detail::Full);
        assert!(t.following());
        assert!(t.total() > before, "{} {before}", t.total());
        t.toggle_detail();
        assert_eq!(t.total(), before);
        // Reading, the row at the top stays at the top.
        let user = t
            .rows
            .iter()
            .position(|row| matches!(row, Row::User(text) if text.starts_with("Run the parser")))
            .unwrap();
        t.anchor = Some(Anchor { row: user, line: 1 });
        t.toggle_detail();
        assert_eq!(t.top(), Anchor { row: user, line: 1 });
        // A line past what the row draws once it shrinks is clamped to it.
        let read = t
            .rows
            .iter()
            .position(|row| matches!(row, Row::Call { tool, .. } if tool == "Read"))
            .unwrap();
        t.anchor = Some(Anchor { row: read, line: 9 });
        t.toggle_detail();
        assert_eq!(t.detail(), Detail::Preview);
        assert_eq!(
            t.top(),
            Anchor {
                row: read,
                line: t.count(read) - 1
            }
        );
        let lines: usize = (0..t.rows.len()).map(|i| t.count(i)).sum();
        assert_eq!(t.total(), lines);
    }

    #[test]
    fn the_newest_cards_count_names_ctrl_o_until_it_is_used() {
        let mut rows = card("Bash", "make", CallState::Done, numbered(30));
        rows.push(Row::Answer("built".into()));
        rows.extend(card("Bash", "make test", CallState::Done, numbered(30)));
        rows.push(Row::Answer("passed".into()));
        let mut t = Transcript::new(rows.clone());
        t.resize(60, 100, PLAIN);
        let hinted = |t: &Transcript| {
            t.visible()
                .iter()
                .map(ToString::to_string)
                .filter(|line| line.contains("Ctrl+O"))
                .collect::<Vec<_>>()
        };
        // Not offered, as inline: no hint.
        assert!(hinted(&t).is_empty());
        let before = t.total();
        t.offer_expand_hint();
        assert_eq!(hinted(&t), ["  │ ⋯ 26 more lines · Ctrl+O"]);
        let newest = t.lines(2);
        let row = newest
            .iter()
            .find(|line| line.to_string().contains("Ctrl+O"));
        assert_eq!(row.unwrap().spans.last().unwrap().style, dim());
        assert_eq!(t.total(), before);
        // Where the count's row has no room for it, it is left out whole.
        t.resize(26, 100, PLAIN);
        assert!(hinted(&t).is_empty());
        let narrow: Vec<String> = t.visible().iter().map(ToString::to_string).collect();
        assert!(narrow.iter().all(|line| !line.contains("Ctrl")));
        assert!(
            narrow.contains(&"  │ ⋯ 26 more lines".to_owned()),
            "{narrow:#?}"
        );
        t.resize(60, 100, PLAIN);
        // Once Ctrl+O is used, it is gone for good, in a new session too.
        t.toggle_detail();
        t.toggle_detail();
        t.offer_expand_hint();
        assert!(hinted(&t).is_empty());
        t.replace(rows);
        t.resize(60, 100, PLAIN);
        t.offer_expand_hint();
        assert!(hinted(&t).is_empty());
        assert_eq!(t.detail(), Detail::Preview);
    }

    // Bake: the hint follows the newest card that has a count, so a short
    // call after a folded one leaves it where it was; a newer folded card
    // takes it. Where that card's count has no room for it, it is left out
    // whole rather than moved to an older card.
    #[test]
    fn the_hint_follows_the_newest_card_with_a_count() {
        let hinted = |t: &Transcript| {
            t.visible()
                .iter()
                .map(ToString::to_string)
                .filter(|line| line.contains("Ctrl+O"))
                .collect::<Vec<_>>()
        };
        let in_row = |t: &Transcript, index: usize| {
            t.lines(index)
                .iter()
                .any(|line| line.to_string().contains("Ctrl+O"))
        };
        let mut rows = card("Bash", "make", CallState::Done, numbered(30));
        rows.push(Row::Answer("built".into()));
        rows.extend(card("Bash", "true", CallState::Done, numbered(2)));
        let mut t = Transcript::new(rows);
        t.offer_expand_hint();
        t.resize(60, 100, PLAIN);
        assert_eq!(hinted(&t), ["  │ ⋯ 26 more lines · Ctrl+O"]);
        assert!(in_row(&t, 0));
        // A newer folded card takes it, and a running one's count too.
        t.commit(card("Bash", "make all", CallState::Done, numbered(1030)));
        t.resize(60, 100, PLAIN);
        let newest = t.rows.len() - 1;
        assert_eq!(hinted(&t), ["  │ ⋯ 1026 more lines · Ctrl+O"]);
        assert!(in_row(&t, newest) && !in_row(&t, 0));
        t.set_live(card("Bash", "watch", CallState::Running, numbered(9)));
        t.resize(60, 100, PLAIN);
        assert_eq!(hinted(&t).len(), 1);
        assert!(in_row(&t, t.rows.len() - 1));
        t.set_live(Vec::new());
        // Too narrow for the newest card's count and the hint: none at all,
        // though the older card's shorter count would have room.
        let older_alone = |width: usize| {
            let mut alone = Transcript::new(card("Bash", "make", CallState::Done, numbered(30)));
            alone.offer_expand_hint();
            alone.resize(width, 100, PLAIN);
            !hinted(&alone).is_empty()
        };
        let width = (20..60)
            .find(|&width| {
                t.resize(width, 100, PLAIN);
                hinted(&t).is_empty() && older_alone(width)
            })
            .expect("a width where only the older count has room");
        t.resize(width, 100, PLAIN);
        assert!(!in_row(&t, 0) && !in_row(&t, newest));
    }

    #[test]
    fn every_card_fits_its_width_expanded() {
        let rows = sample_session();
        for width in [40, 60, 80, 100, 120, 160] {
            for look in [
                PLAIN,
                Look {
                    classic: true,
                    ..PLAIN
                },
                Look {
                    tones: Tones::TrueColor,
                    ..PLAIN
                },
            ] {
                for index in 0..rows.len() {
                    for line in present_detail(&rows, index, width, look, Detail::Full) {
                        assert!(line.width() <= width, "{width} {look:?}: {line:?}");
                        let line = line.to_string();
                        assert!(!line.contains(char::is_control), "{line:?}");
                        if look.classic {
                            assert!(line.is_ascii() || line.contains('·'), "{line:?}");
                        }
                    }
                }
            }
        }
    }

    mod cards_hold {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            // Any output, in any card, settled or not, previewed or whole,
            // draws without a panic inside its width, with no control
            // character, within its bound.
            #[test]
            fn any_output_draws_inside_its_width_and_bound(
                output in prop::collection::vec("[a-z:+@ {}\\[\\]\",.0-9\u{1b}\t\\-]{0,60}|.{0,60}", 0..30),
                tool in prop::sample::select(vec!["Bash", "Read", "Edit", "Write", "Grep", "Find", "Fetch", "WebSearch", "Notes"]),
                state in prop::sample::select(vec![CallState::Running, CallState::Done, CallState::Failed]),
                width in 20usize..140,
                tones in prop::sample::select(vec![Tones::None, Tones::Ansi, Tones::TrueColor]),
                classic in any::<bool>(),
            ) {
                let rows = vec![Row::Call {
                    tool: tool.into(),
                    argument: "src/a.rs:3-9".into(),
                    fields: Vec::new(),
                    id: None,
                    state,
                    summary: None,
                    output,
                    full_output_path: Some("/tmp/out\u{1b}[2J.log".into()),
                }];
                let look = Look { tones, classic, lit: true };
                for detail in [Detail::Preview, Detail::Full] {
                    let lines = present_detail(&rows, 0, width, look, detail);
                    prop_assert!(lines.len() <= 4 + EXPANDED_ROWS + 4, "{}", lines.len());
                    for line in &lines {
                        prop_assert!(line.width() <= width, "{width}: {line:?}");
                        prop_assert!(!line.to_string().contains(char::is_control), "{line:?}");
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "known: a collapsed tab before a zero-width extender merges with a hanging space; see fuzz tui_text"]
    fn a_collapsed_tab_keeps_a_wrapped_row_within_its_width() {
        use unicode_segmentation::UnicodeSegmentation;
        // Found by the `tui_text` fuzz target: the space and the tab hang
        // past the one-cell row, the tab is drawn as no spaces, and the
        // space then joins the zero-width joiner into a one-cell grapheme.
        for row in wrap("\u{FFFD} \t\u{200D}", 1) {
            let cells: usize = row.graphemes(true).map(editor::cell_width).sum();
            assert!(cells <= 1, "wrapped row {row:?} takes {cells} cells");
        }
    }
}
