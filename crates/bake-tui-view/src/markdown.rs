//! The model's answer drawn as Markdown: headings, emphasis, inline code,
//! links, lists and task lists, block quotes, rules, fenced code coloured
//! by [`crate::syntax`] where it knows the language, and tables.
//!
//! The parser is small, line-based, and total: any text draws, markup it
//! does not understand reads as the text it is, and an answer that is still
//! streaming, such as one with a fence not yet closed, draws as far as it
//! goes. Each source line stays its own line, as a chat message's line
//! breaks do. Control characters are dropped before anything is drawn, so
//! no answer can become a terminal escape sequence.
//!
//! Colour follows the terminal's [`Tones`]: `NO_COLOR` keeps the bold,
//! italic, and underline but no colour, and the classic frame draws its
//! bullets, bars, and borders in ASCII.

use std::ops::Range;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::editor;
use crate::syntax::{Carry, Lang};
use crate::transcript::{self, Look, RAIL};

/// Nested emphasis deeper than this reads as text.
const MAX_DEPTH: usize = 8;
/// The most a list item or paragraph line is indented, in cells.
const MAX_INDENT: usize = 12;

/// The lines `text` draws as Markdown in `width` cells, each opening with
/// the transcript's rail.
pub fn present(text: &str, width: usize, look: Look) -> Vec<Line<'static>> {
    let clean = editor::sanitize(text);
    let lines: Vec<String> = clean.split('\n').map(transcript::expand_tabs).collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut gap = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].as_str();
        if line.trim().is_empty() {
            gap = !out.is_empty();
            i += 1;
            continue;
        }
        if gap {
            out.push(Line::default());
            gap = false;
        }
        if let Some((fence, info)) = fence_open(line) {
            let start = i + 1;
            let mut end = start;
            while end < lines.len() && !fence_closes(&lines[end], &fence) {
                end += 1;
            }
            out.extend(code_block(&lines[start..end], info, width, look));
            i = (end + 1).min(lines.len());
            continue;
        }
        if let Some(rows) = table_rows(&lines[i..]) {
            out.extend(table(&lines[i..i + rows], width, look));
            i += rows;
            continue;
        }
        out.extend(block_line(line, width, look));
        i += 1;
    }
    if out.is_empty() {
        out.push(Line::from(vec![transcript::pad(RAIL)]));
    }
    out
}

/// Text as drawn and the style of every byte, in order and without
/// overlaps.
type Styled = (String, Vec<(Range<usize>, Style)>);

/// The styles the answer draws in.
struct Palette {
    heading: Style,
    subheading: Style,
    code: Style,
    link: Style,
    marker: Style,
    quote: Style,
    border: Style,
    done: Style,
}

fn palette(look: Look) -> Palette {
    let colour = |rgb, ansi| transcript::colour(look.tones, rgb, ansi);
    let accent = transcript::accent(look.tones);
    Palette {
        heading: accent.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        subheading: accent.add_modifier(Modifier::BOLD),
        code: colour((0xc4, 0xb5, 0xfd), Color::Magenta),
        link: colour((0x60, 0xa5, 0xfa), Color::Blue).add_modifier(Modifier::UNDERLINED),
        marker: accent,
        quote: colour((0x6b, 0x72, 0x80), Color::DarkGray),
        border: transcript::dim(),
        done: colour((0x22, 0xc5, 0x5e), Color::Green),
    }
}

/// The glyphs the answer draws, ASCII under the classic frame.
struct Glyphs {
    bullets: [&'static str; 3],
    rule: &'static str,
    bar: &'static str,
    code_top: &'static str,
    code_bottom: &'static str,
    column: &'static str,
    cross: &'static str,
    checked: &'static str,
}

fn glyphs(look: Look) -> Glyphs {
    if look.classic {
        Glyphs {
            bullets: ["-", "*", "+"],
            rule: "-",
            bar: "| ",
            code_top: "+-",
            code_bottom: "+-",
            column: "|",
            cross: "+",
            checked: "[x]",
        }
    } else {
        Glyphs {
            bullets: ["•", "◦", "▪"],
            rule: "─",
            bar: "│ ",
            code_top: "╭─",
            code_bottom: "╰─",
            column: "│",
            cross: "┼",
            checked: "[✓]",
        }
    }
}

/// One line outside a fence or table: a heading, rule, quote, list item,
/// or paragraph line.
fn block_line(line: &str, width: usize, look: Look) -> Vec<Line<'static>> {
    let colours = palette(look);
    let marks = glyphs(look);
    let rail = || vec![transcript::pad(RAIL)];
    let indent = leading(line);
    let trimmed = line.trim_start();
    if indent < 4 {
        if let Some((level, title)) = heading(trimmed) {
            let style = if level <= 1 {
                colours.heading
            } else if level == 2 {
                colours.subheading
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            };
            return hang_inline(title, style, width, RAIL, rail(), rail(), look);
        }
        if is_rule(trimmed) {
            let cells = width.saturating_sub(2 * RAIL).max(1);
            return vec![Line::from(vec![
                transcript::pad(RAIL),
                Span::styled(marks.rule.repeat(cells), colours.border),
            ])];
        }
    }
    if trimmed.starts_with('>') {
        let (depth, rest) = quote(trimmed);
        let mut lead = rail();
        for _ in 0..depth {
            lead.push(Span::styled(marks.bar, colours.quote));
        }
        let cells = RAIL + depth * marks.bar.width();
        let style = Style::new().add_modifier(Modifier::ITALIC);
        return hang_inline(rest, style, width, cells, lead.clone(), lead, look);
    }
    // Indentation never takes more than a quarter of the row.
    let indent = indent.min(MAX_INDENT).min(width / 4);
    if let Some((marker, rest)) = list_marker(trimmed) {
        let (mark, mark_style, rest) = match (marker, task(rest)) {
            (_, Some((true, rest))) => (marks.checked.to_owned(), colours.done, rest),
            (_, Some((false, rest))) => ("[ ]".to_owned(), colours.border, rest),
            (Marker::Bullet, None) => (
                marks.bullets[(indent / 2) % marks.bullets.len()].to_owned(),
                colours.marker,
                rest,
            ),
            (Marker::Ordered(number), None) => (number.to_owned(), colours.marker, rest),
        };
        let mut first = rail();
        first.push(transcript::pad(indent));
        first.push(Span::styled(mark.clone(), mark_style));
        first.push(transcript::pad(1));
        let cells = RAIL + indent + mark.width() + 1;
        let rest_lead = vec![transcript::pad(cells)];
        return hang_inline(rest, Style::new(), width, cells, first, rest_lead, look);
    }
    let lead = vec![transcript::pad(RAIL + indent)];
    hang_inline(
        trimmed,
        Style::new(),
        width,
        RAIL + indent,
        lead.clone(),
        lead,
        look,
    )
}

/// `text`'s inline markup drawn over `base` and wrapped at `width` less
/// `indent`, the first row opening with `first` and the rest with `rest`.
fn hang_inline(
    text: &str,
    base: Style,
    width: usize,
    indent: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    look: Look,
) -> Vec<Line<'static>> {
    let (shown, runs) = inline(text, base, look);
    transcript::hang_runs(&shown, &runs, Style::new(), width, indent, first, rest)
}

/// Cells of leading spaces.
fn leading(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// An ATX heading: its level and title, closing `#`s left out.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let title = rest.trim();
    // Closing hashes count only after a space; `C#` keeps its hash.
    let stripped = title.trim_end_matches('#');
    let title = if stripped.is_empty() || stripped.ends_with(' ') {
        stripped.trim_end()
    } else {
        title
    };
    Some((level, title))
}

/// A thematic break: three or more of one of `-`, `*`, or `_`, spaces
/// allowed between.
fn is_rule(line: &str) -> bool {
    let mut marks = line.chars().filter(|c| *c != ' ');
    let Some(first) = marks.next() else {
        return false;
    };
    matches!(first, '-' | '*' | '_') && marks.clone().all(|c| c == first) && marks.count() >= 2
}

/// A block quote's depth and its text.
fn quote(line: &str) -> (usize, &str) {
    let mut depth = 0;
    let mut rest = line;
    while let Some(after) = rest.strip_prefix('>') {
        depth += 1;
        rest = after
            .strip_prefix(' ')
            .unwrap_or(after)
            .trim_start_matches(' ');
        if depth == 4 {
            break;
        }
    }
    (depth, rest)
}

/// What opens a list item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker<'a> {
    Bullet,
    /// Its number and delimiter as written, such as `1.`.
    Ordered(&'a str),
}

/// A list item's marker and its text.
fn list_marker(line: &str) -> Option<(Marker<'_>, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some((Marker::Bullet, rest.trim_start_matches(' ')));
        }
    }
    if matches!(line, "-" | "*" | "+") {
        return Some((Marker::Bullet, ""));
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let delimiter = line.as_bytes().get(digits).copied();
        let after = line.as_bytes().get(digits + 1).copied();
        if matches!(delimiter, Some(b'.' | b')')) && matches!(after, None | Some(b' ')) {
            let rest = line[digits + 1..].trim_start_matches(' ');
            return Some((Marker::Ordered(&line[..=digits]), rest));
        }
    }
    None
}

/// A task item's box: whether it is checked, and the text after it.
fn task(rest: &str) -> Option<(bool, &str)> {
    let checked = match rest.get(..3)? {
        "[ ]" => false,
        "[x]" | "[X]" => true,
        _ => return None,
    };
    let after = &rest[3..];
    (after.is_empty() || after.starts_with(' ')).then(|| (checked, after.trim_start()))
}

/// A fence that opens a code block: its marker and its info string.
fn fence_open(line: &str) -> Option<(String, &str)> {
    if leading(line) > 3 {
        return None;
    }
    let trimmed = line.trim_start();
    let mark = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let count = trimmed.chars().take_while(|c| *c == mark).count();
    if count < 3 {
        return None;
    }
    let info = trimmed[count..].trim();
    if mark == '`' && info.contains('`') {
        return None;
    }
    Some((trimmed[..count].to_owned(), info))
}

/// Whether `line` closes the block `fence` opened.
fn fence_closes(line: &str, fence: &str) -> bool {
    let trimmed = line.trim();
    let Some(mark) = fence.chars().next() else {
        return false;
    };
    let count = trimmed.chars().take_while(|c| *c == mark).count();
    count >= fence.len() && trimmed.len() == count * mark.len_utf8()
}

/// The language a fence's info string names.
fn fence_lang(info: &str) -> Option<Lang> {
    let name = info.split_whitespace().next()?.to_ascii_lowercase();
    match name.as_str() {
        "ts" | "tsx" | "typescript" | "js" | "jsx" | "javascript" | "mjs" | "cjs" | "json"
        | "jsonc" => Some(Lang::TypeScript),
        "rs" | "rust" => Some(Lang::Rust),
        "sh" | "bash" | "zsh" | "shell" | "console" | "shellsession" => Some(Lang::Shell),
        _ => None,
    }
}

/// A fenced block: a top edge naming its language, the code under a bar,
/// coloured where its language is known, and a bottom edge.
fn code_block(code: &[String], info: &str, width: usize, look: Look) -> Vec<Line<'static>> {
    let colours = palette(look);
    let marks = glyphs(look);
    let label = info.split_whitespace().next().unwrap_or("");
    let mut top = vec![
        transcript::pad(RAIL),
        Span::styled(marks.code_top, colours.border),
    ];
    if !label.is_empty() {
        top.push(Span::styled(
            format!(" {}", fit(label, width.saturating_sub(RAIL + 3))),
            colours.border.add_modifier(Modifier::ITALIC),
        ));
    }
    let mut lines = vec![Line::from(top)];
    let lang = fence_lang(info);
    let mut carry = Carry::default();
    let lead = vec![
        transcript::pad(RAIL),
        Span::styled(marks.bar, colours.border),
    ];
    let indent = RAIL + marks.bar.width();
    for line in code {
        let runs = transcript::code_runs(line, lang, &mut carry, look.tones);
        lines.extend(transcript::hang_runs(
            line,
            &runs,
            Style::new(),
            width,
            indent,
            lead.clone(),
            lead.clone(),
        ));
    }
    lines.push(Line::from(vec![
        transcript::pad(RAIL),
        Span::styled(marks.code_bottom, colours.border),
    ]));
    lines
}

/// `text` cut to `cells`, ending in `…` when cut.
fn fit(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + c.to_string().width() + 1 > cells {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// A table's cells, the outer pipes left out; `\|` stays in its cell.
fn cells(line: &str) -> Vec<&str> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed
        .strip_suffix('|')
        .filter(|rest| !rest.ends_with('\\'))
        .unwrap_or(trimmed);
    let mut cells = Vec::new();
    let mut start = 0;
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'|' => {
                cells.push(trimmed[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    cells.push(trimmed[start.min(trimmed.len())..].trim());
    cells
}

/// How a table column aligns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

/// The alignments of a delimiter row, or `None` when `line` is not one.
fn delimiters(line: &str) -> Option<Vec<Align>> {
    if !line.contains('|') && !line.contains('-') {
        return None;
    }
    let row = cells(line);
    row.iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            let dashes = cell.trim_matches(':');
            (!dashes.is_empty() && dashes.bytes().all(|b| b == b'-')).then_some(
                match (left, right) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                },
            )
        })
        .collect()
}

/// How many of `lines` a table starting at the first takes: a header row
/// with a pipe, a delimiter row, and the rows with a pipe after them.
fn table_rows(lines: &[String]) -> Option<usize> {
    let header = lines.first()?;
    let delimiter = lines.get(1)?;
    if !header.contains('|') || !delimiter.contains('|') || leading(header) > 3 {
        return None;
    }
    let aligns = delimiters(delimiter)?;
    if aligns.len() != cells(header).len() {
        return None;
    }
    let body = lines[2..]
        .iter()
        .take_while(|line| line.contains('|') && !line.trim().is_empty())
        .count();
    Some(2 + body)
}

/// A table with its columns aligned, its header bold, and a rule under
/// it; one too wide for `width` draws each row as `header: value` pairs.
fn table(lines: &[String], width: usize, look: Look) -> Vec<Line<'static>> {
    let colours = palette(look);
    let marks = glyphs(look);
    let aligns = delimiters(&lines[1]).unwrap_or_default();
    let columns = aligns.len();
    let render = |line: &str, base: Style| -> Vec<Styled> {
        let mut row: Vec<_> = cells(line)
            .into_iter()
            .take(columns)
            .map(|cell| inline(cell, base, look))
            .collect();
        row.resize_with(columns, || (String::new(), Vec::new()));
        row
    };
    let header = render(&lines[0], Style::new().add_modifier(Modifier::BOLD));
    let body: Vec<_> = lines[2..]
        .iter()
        .map(|line| render(line, Style::new()))
        .collect();
    let mut widths = vec![0; columns];
    for row in std::iter::once(&header).chain(&body) {
        for (cell, w) in row.iter().zip(&mut widths) {
            *w = (*w).max(cell.0.width());
        }
    }
    let separator = format!(" {} ", marks.column);
    let total = RAIL + widths.iter().sum::<usize>() + separator.width() * columns.saturating_sub(1);
    if total > width || columns == 0 {
        return narrow_table(&header, &body, width, look);
    }
    let draw = |row: &[Styled]| -> Line<'static> {
        let mut spans = vec![transcript::pad(RAIL)];
        for (index, (text, runs)) in row.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(separator.clone(), colours.border));
            }
            let room = widths[index] - text.width();
            let (before, after) = match aligns[index] {
                Align::Left => (0, room),
                Align::Right => (room, 0),
                Align::Center => (room / 2, room - room / 2),
            };
            spans.push(transcript::pad(before));
            spans.extend(styled_spans(text, runs));
            if index + 1 < row.len() {
                spans.push(transcript::pad(after));
            }
        }
        Line::from(spans)
    };
    let mut out = vec![draw(&header)];
    let rule: Vec<String> = widths.iter().map(|w| marks.rule.repeat(*w)).collect();
    let joint = format!("{}{}{}", marks.rule, marks.cross, marks.rule);
    out.push(Line::from(vec![
        transcript::pad(RAIL),
        Span::styled(rule.join(&joint), colours.border),
    ]));
    out.extend(body.iter().map(|row| draw(row)));
    out
}

/// A table too wide to align: each row as its header's names, dim, before
/// each value, the rows bulleted.
fn narrow_table(
    header: &[Styled],
    body: &[Vec<Styled>],
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let colours = palette(look);
    let marks = glyphs(look);
    let mut out = Vec::new();
    for row in body {
        let mut text = String::new();
        let mut runs = Vec::new();
        for (index, ((name, _), (value, value_runs))) in header.iter().zip(row).enumerate() {
            if index > 0 {
                push_run(&mut text, &mut runs, " · ", colours.border);
            }
            if !name.is_empty() {
                push_run(
                    &mut text,
                    &mut runs,
                    &format!("{name}: "),
                    transcript::dim(),
                );
            }
            let at = text.len();
            text.push_str(value);
            runs.extend(
                value_runs
                    .iter()
                    .map(|(range, style)| (range.start + at..range.end + at, *style)),
            );
        }
        let bullet = marks.bullets[0];
        let first = vec![
            transcript::pad(RAIL),
            Span::styled(bullet, colours.marker),
            transcript::pad(1),
        ];
        let cells = RAIL + bullet.width() + 1;
        out.extend(transcript::hang_runs(
            &text,
            &runs,
            Style::new(),
            width,
            cells,
            first,
            vec![transcript::pad(cells)],
        ));
    }
    out
}

/// `text` as spans, each run in its style and the rest plain.
fn styled_spans(text: &str, runs: &[(Range<usize>, Style)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut at = 0;
    for (range, style) in runs {
        if range.start > at {
            spans.push(Span::raw(text[at..range.start].to_owned()));
        }
        spans.push(Span::styled(text[range.clone()].to_owned(), *style));
        at = range.end;
    }
    if at < text.len() {
        spans.push(Span::raw(text[at..].to_owned()));
    }
    spans
}

/// Appends `piece` in `style`, joining the last run when it has the style.
fn push_run(text: &mut String, runs: &mut Vec<(Range<usize>, Style)>, piece: &str, style: Style) {
    if piece.is_empty() {
        return;
    }
    let start = text.len();
    text.push_str(piece);
    if let Some((range, last)) = runs.last_mut()
        && *last == style
        && range.end == start
    {
        range.end = text.len();
        return;
    }
    runs.push((start..text.len(), style));
}

/// `text`'s inline markup: the text as drawn, markers left out, and the
/// style of every byte, in order and without overlaps.
pub(crate) fn inline(text: &str, base: Style, look: Look) -> Styled {
    let mut out = Inline {
        text: String::with_capacity(text.len()),
        runs: Vec::new(),
        colours: palette(look),
    };
    out.parse(text, base, 0);
    (out.text, out.runs)
}

struct Inline {
    text: String,
    runs: Vec<(Range<usize>, Style)>,
    colours: Palette,
}

impl Inline {
    fn push(&mut self, piece: &str, style: Style) {
        push_run(&mut self.text, &mut self.runs, piece, style);
    }

    fn parse(&mut self, text: &str, style: Style, depth: usize) {
        let bytes = text.as_bytes();
        let mut plain = 0;
        let mut i = 0;
        while i < text.len() {
            let taken = match bytes[i] {
                b'\\' => match text[i + 1..].chars().next() {
                    Some(next) if next.is_ascii_punctuation() => {
                        self.push(&text[plain..i], style);
                        self.push(&text[i + 1..i + 2], style);
                        Some(i + 2)
                    }
                    _ => None,
                },
                b'`' => self.code(text, i, plain, style),
                b'*' | b'_' | b'~' if depth < MAX_DEPTH => {
                    self.emphasis(text, i, plain, style, depth)
                }
                b'[' => self.link(text, i, i, plain, style, depth),
                b'!' if bytes.get(i + 1) == Some(&b'[') => {
                    self.link(text, i, i + 1, plain, style, depth)
                }
                b'<' => self.autolink(text, i, plain, style),
                b'h' => self.bare_url(text, i, plain, style),
                _ => None,
            };
            match taken {
                Some(next) => {
                    i = next;
                    plain = next;
                }
                None => {
                    // A run of markers that matched nothing reads as text
                    // whole, so its tail is not taken for an opening.
                    let c = bytes[i];
                    i += if matches!(c, b'*' | b'_' | b'~' | b'`') {
                        bytes[i..].iter().take_while(|&&b| b == c).count()
                    } else {
                        text[i..].chars().next().map_or(1, char::len_utf8)
                    };
                }
            }
        }
        self.push(&text[plain..], style);
    }

    /// A code span opening at `at`: the end of its closing run.
    fn code(&mut self, text: &str, at: usize, plain: usize, style: Style) -> Option<usize> {
        let ticks = run(text, at, b'`');
        let mut search = at + ticks;
        while let Some(found) = text[search..].find('`') {
            let close = search + found;
            let length = run(text, close, b'`');
            if length == ticks {
                let inner = &text[at + ticks..close];
                let inner = if inner.len() >= 2
                    && inner.starts_with(' ')
                    && inner.ends_with(' ')
                    && !inner.trim().is_empty()
                {
                    &inner[1..inner.len() - 1]
                } else {
                    inner
                };
                self.push(&text[plain..at], style);
                self.push(inner, style.patch(self.colours.code));
                return Some(close + ticks);
            }
            search = close + length;
        }
        None
    }

    /// Emphasis opening at `at`: `*em*`, `**strong**`, `***both***`, their
    /// `_` forms between word boundaries, and `~~struck~~`.
    fn emphasis(
        &mut self,
        text: &str,
        at: usize,
        plain: usize,
        style: Style,
        depth: usize,
    ) -> Option<usize> {
        let mark = text.as_bytes()[at];
        let opening = run(text, at, mark);
        let lengths: &[usize] = if mark == b'~' { &[2] } else { &[3, 2, 1] };
        let before = text[..at].chars().next_back();
        if mark == b'_' && before.is_some_and(char::is_alphanumeric) {
            return None;
        }
        for &length in lengths.iter().filter(|&&length| length == opening) {
            let after = text[at + length..].chars().next()?;
            if after.is_whitespace() {
                return None;
            }
            let mut search = at + length;
            while let Some(found) = text[search..].find(char::from(mark)) {
                let close = search + found;
                let closing = run(text, close, mark);
                let inner = &text[at + length..close];
                let tail = text[close + closing..].chars().next();
                let fits = closing == length
                    && !inner.is_empty()
                    && !inner.ends_with(char::is_whitespace)
                    && !(mark == b'_' && tail.is_some_and(char::is_alphanumeric));
                if fits {
                    let modifier = match (mark, length) {
                        (b'~', _) => Modifier::CROSSED_OUT,
                        (_, 1) => Modifier::ITALIC,
                        (_, 2) => Modifier::BOLD,
                        _ => Modifier::BOLD | Modifier::ITALIC,
                    };
                    self.push(&text[plain..at], style);
                    self.parse(inner, style.add_modifier(modifier), depth + 1);
                    return Some(close + closing);
                }
                search = close + closing;
            }
        }
        None
    }

    /// A link or image whose `[` is at `open`, the whole starting at `at`:
    /// its label in the link style, then its address, dim, when the label
    /// is not the address itself.
    fn link(
        &mut self,
        text: &str,
        at: usize,
        open: usize,
        plain: usize,
        style: Style,
        depth: usize,
    ) -> Option<usize> {
        let close = matching(text, open, b'[', b']')?;
        if text.as_bytes().get(close + 1) != Some(&b'(') {
            return None;
        }
        let end = matching(text, close + 1, b'(', b')')?;
        let label = &text[open + 1..close];
        let target = text[close + 2..end].split_whitespace().next().unwrap_or("");
        let target = target.trim_start_matches('<').trim_end_matches('>');
        self.push(&text[plain..at], style);
        let image = at != open;
        let label_style = if image {
            style
                .patch(transcript::dim())
                .add_modifier(Modifier::ITALIC)
        } else {
            style.patch(self.colours.link)
        };
        if label.is_empty() {
            self.push(if image { "image" } else { target }, label_style);
        } else {
            self.parse(label, label_style, depth + 1);
        }
        if !target.is_empty() && target != label {
            self.push(&format!(" ({target})"), style.patch(transcript::dim()));
        }
        Some(end + 1)
    }

    /// `<https://…>` or `<mailto:…>`, drawn as a link without its brackets.
    fn autolink(&mut self, text: &str, at: usize, plain: usize, style: Style) -> Option<usize> {
        let close = at + text[at..].find('>')?;
        let inner = &text[at + 1..close];
        let linked = ["http://", "https://", "mailto:"]
            .iter()
            .any(|scheme| inner.starts_with(scheme));
        if !linked || inner.contains(char::is_whitespace) {
            return None;
        }
        self.push(&text[plain..at], style);
        self.push(inner, style.patch(self.colours.link));
        Some(close + 1)
    }

    /// A bare `http://` or `https://` address, ending before whitespace and
    /// the punctuation a sentence puts after it.
    fn bare_url(&mut self, text: &str, at: usize, plain: usize, style: Style) -> Option<usize> {
        let rest = &text[at..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            return None;
        }
        if text[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
        {
            return None;
        }
        let mut end = rest
            .find(|c: char| c.is_whitespace() || c == '<' || c == '`')
            .unwrap_or(rest.len());
        while end > 0 {
            let last = rest[..end].chars().next_back()?;
            let unbalanced =
                last == ')' && rest[..end].matches('(').count() < rest[..end].matches(')').count();
            if matches!(
                last,
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '_'
            ) || unbalanced
            {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        if !rest[..end].contains("://") || rest[..end].ends_with("://") {
            return None;
        }
        self.push(&text[plain..at], style);
        self.push(&rest[..end], style.patch(self.colours.link));
        Some(at + end)
    }
}

/// The length of the run of `mark` bytes at `at`.
fn run(text: &str, at: usize, mark: u8) -> usize {
    text.as_bytes()[at..]
        .iter()
        .take_while(|&&b| b == mark)
        .count()
}

/// Where the `close` byte matching the `open` byte at `at` is, nesting
/// counted and escapes and code spans skipped.
fn matching(text: &str, at: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = at;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'`' => {
                let ticks = run(text, i, b'`');
                let after = i + ticks;
                match text[after..].find(&"`".repeat(ticks)) {
                    Some(found) => i = after + found + ticks - 1,
                    None => i = after - 1,
                }
            }
            b if b == open => depth += 1,
            b if b == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Tones;
    use proptest::prelude::*;

    const PLAIN: Look = Look {
        tones: Tones::None,
        classic: false,
        lit: true,
    };
    const COLOUR: Look = Look {
        tones: Tones::TrueColor,
        classic: false,
        lit: true,
    };

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn drawn(markdown: &str, width: usize) -> Vec<String> {
        text(&present(markdown, width, PLAIN))
    }

    /// The style of the first span holding `needle`.
    fn style_of(lines: &[Line], needle: &str) -> Style {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {lines:#?}"))
            .style
    }

    #[test]
    fn inline_markers_are_left_out_and_style_their_text() {
        let lines = present(
            "Use **bold**, *it*, `code`, ~~gone~~, and [docs](https://x.dev).",
            80,
            COLOUR,
        );
        assert_eq!(
            text(&lines),
            ["  Use bold, it, code, gone, and docs (https://x.dev)."]
        );
        assert!(
            style_of(&lines, "bold")
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            style_of(&lines, "it")
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert_eq!(style_of(&lines, "code").fg, palette(COLOUR).code.fg);
        assert!(
            style_of(&lines, "gone")
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
        assert!(
            style_of(&lines, "docs")
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn markup_that_does_not_close_reads_as_text() {
        assert_eq!(
            drawn("a ** b, snake_case_name, 2 * 3 * 4, `open, [x](y", 80),
            ["  a ** b, snake_case_name, 2 * 3 * 4, `open, [x](y"]
        );
        assert_eq!(
            drawn(r"\*not em\* and \`tick\`", 80),
            ["  *not em* and `tick`"]
        );
    }

    #[test]
    fn nested_emphasis_and_code_inside_a_link() {
        let lines = present("**bold *both* bold** [`api`](https://a.b)", 80, COLOUR);
        assert_eq!(text(&lines), ["  bold both bold api (https://a.b)"]);
        let both = style_of(&lines, "both").add_modifier;
        assert!(both.contains(Modifier::BOLD | Modifier::ITALIC));
        let api = style_of(&lines, "api");
        assert!(api.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(api.fg, palette(COLOUR).code.fg);
    }

    #[test]
    fn bare_and_angle_urls_are_links_without_trailing_punctuation() {
        let lines = present(
            "See https://a.dev/x. Or <https://b.dev>, (https://c.dev/(y))",
            80,
            COLOUR,
        );
        assert_eq!(
            text(&lines),
            ["  See https://a.dev/x. Or https://b.dev, (https://c.dev/(y))"]
        );
        for url in ["https://a.dev/x", "https://b.dev", "https://c.dev/(y)"] {
            let span = style_of(&lines, url);
            assert!(span.add_modifier.contains(Modifier::UNDERLINED), "{url}");
        }
        let after = lines[0]
            .spans
            .iter()
            .find(|span| span.content.starts_with(". Or"))
            .expect("period");
        assert!(!after.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn headings_rules_and_quotes() {
        let lines = present(
            "# Title\n## Part #\n### C#\n---\n> quoted *words*\n>> deeper",
            20,
            COLOUR,
        );
        assert_eq!(
            text(&lines),
            [
                "  Title",
                "  Part",
                "  C#",
                &format!("  {}", "─".repeat(16)),
                "  │ quoted words",
                "  │ │ deeper",
            ]
        );
        assert!(
            style_of(&lines, "Title")
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert!(
            style_of(&lines, "Part")
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn lists_hang_their_wrapped_rows_under_the_text() {
        assert_eq!(
            drawn(
                "- one two three four\n  - nested item\n3. third\n- [x] done\n- [ ] todo",
                16
            ),
            [
                "  • one two",
                "    three four",
                "    ◦ nested",
                "      item",
                "  3. third",
                "  [✓] done",
                "  [ ] todo",
            ]
        );
        let classic = Look {
            classic: true,
            ..PLAIN
        };
        assert_eq!(
            text(&present("- a\n- [x] b", 20, classic)),
            ["  - a", "  [x] b"]
        );
    }

    #[test]
    fn a_fence_draws_a_framed_coloured_block_and_keeps_markup_literal() {
        let lines = present(
            "Run:\n\n```rust\nlet x = \"**not bold**\";\n```\nafter",
            40,
            COLOUR,
        );
        assert_eq!(
            text(&lines),
            [
                "  Run:",
                "",
                "  ╭─ rust",
                "  │ let x = \"**not bold**\";",
                "  ╰─",
                "  after",
            ]
        );
        let keyword = style_of(&lines, "let");
        assert!(keyword.fg.is_some(), "{keyword:?}");
        assert!(
            !style_of(&lines, "not bold")
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn an_unclosed_fence_while_streaming_draws_to_the_end() {
        assert_eq!(
            drawn("```\ncode # one\n- two", 40),
            ["  ╭─", "  │ code # one", "  │ - two", "  ╰─"]
        );
    }

    #[test]
    fn a_table_aligns_its_columns_and_a_narrow_one_reads_as_pairs() {
        let table = "| Name | Size |\n|:-----|-----:|\n| `a.rs` | 12 |\n| b | 3456 |";
        assert_eq!(
            drawn(table, 40),
            [
                "  Name │ Size",
                "  ─────┼─────",
                "  a.rs │   12",
                "  b    │ 3456",
            ]
        );
        assert_eq!(
            drawn(table, 10),
            [
                "  • Name:",
                "    a.rs ·",
                "    Size:",
                "    12",
                "  • Name:",
                "    b ·",
                "    Size:",
                "    3456",
            ]
        );
    }

    #[test]
    fn blank_runs_collapse_and_control_characters_are_dropped() {
        assert_eq!(drawn("a\n\n\n\nb\x1b[31m\n\n", 20), ["  a", "", "  b[31m"]);
        assert_eq!(drawn("", 20), [""]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        // Any text draws without a panic, inside its width, with no
        // control character on screen.
        #[test]
        fn any_text_draws_inside_its_width(
            source in "[ -~\n\t]{0,200}|[*_`~\\[\\]()<>|#> -]{0,80}|.{0,60}",
            width in 20usize..60,
        ) {
            for look in [PLAIN, COLOUR] {
                let lines = present(&source, width, look);
                for line in &lines {
                    prop_assert!(line.width() <= width, "{line:?} > {width}");
                    for span in &line.spans {
                        prop_assert!(!span.content.contains(|c: char| c.is_control()), "{span:?}");
                    }
                }
            }
        }

        // Inline runs are in order, inside the text, and never overlap.
        #[test]
        fn inline_runs_partition_the_text(source in "[a-z *_`~\\[\\]()<>:/.h]{0,80}") {
            let (shown, runs) = inline(&source, Style::new(), COLOUR);
            let mut at = 0;
            for (range, _) in &runs {
                prop_assert!(range.start >= at && range.end <= shown.len() && range.start < range.end);
                prop_assert!(shown.is_char_boundary(range.start) && shown.is_char_boundary(range.end));
                at = range.end;
            }
        }
    }
}
