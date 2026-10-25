//! A `codemode` call as one transcript block, after Pi's codemode renderer.
//!
//! Adapted from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT;
//! see the crate's `NOTICE`): `src/extensions/codemode/renderer.ts` and the
//! cases of `test/codemode-renderer.test.ts`. The block shows the script,
//! then one row per tool call the script made, with its status, duration,
//! and error, then what went back to the model without the result's
//! `Script completed` header.
//!
//! Bake draws it in the transcript's grammar:
//!
//! - A `{} codemode:` head whose status counts the calls: how many run while
//!   the script runs, and how many failed or were cancelled once it ended.
//!   It ends with how long the script took, Pi's `Took`, read from the
//!   result's `Wall time` line, so a resumed transcript reads the same.
//! - The script, numbered and coloured as JavaScript, and the box, marks,
//!   and gutter of any call.
//! - No expand key, so every part stays as Pi's collapsed view draws it,
//!   apart from a failed call's error, which Pi shows only expanded.
//!
//! Pi's model-call costs are left out: Bake's code mode has no `models`
//! (D42). Nothing here keeps a clock, so a running block shows no `Elapsed`
//! time; the bar's activity carries the seconds.

use std::time::Duration;

use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity::Tones;
use crate::copy;
use crate::editor;
use crate::shell_output;
use crate::syntax::{Carry, Lang};
use crate::transcript::{self, BODY, CallState, Look, RAIL};

/// Script rows the block draws before it counts the rest; Pi's
/// `CODE_PREVIEW_LINES`.
pub const CODE_PREVIEW_LINES: usize = 10;
/// Calls the block lists, the latest ones; Pi's `CALL_PREVIEW_COUNT`.
pub const CALL_PREVIEW_COUNT: usize = 8;
/// Output rows the block draws before it counts the rest; Pi's
/// `OUTPUT_PREVIEW_LINES`.
pub const OUTPUT_PREVIEW_LINES: usize = 5;
/// Characters of a call's arguments a row shows; Pi's
/// `COLLAPSED_ARGS_CHARS`.
pub const COLLAPSED_ARGS_CHARS: usize = 80;
/// Rows of a failed or cancelled call's error the block draws before it
/// counts the rest. Bake's own: Pi shows errors only expanded, so eight
/// failed calls stay a few rows each on a narrow terminal.
pub const ERROR_PREVIEW_ROWS: usize = 2;

/// How a call the script made stands; Pi's `CodemodeNestedCall` status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NestedStatus {
    Running,
    Ok,
    Error,
    /// Cut off by the script ending, a timeout, or an abort.
    Cancelled,
}

/// One call the script made, as the tool's `details.calls` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedCall {
    /// The tool's name.
    pub name: String,
    /// Compact JSON of the arguments.
    pub args: String,
    pub status: NestedStatus,
    /// How long it ran, once it settled.
    pub duration: Option<Duration>,
    pub error: Option<String>,
}

/// A `codemode` call and what its result says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Codemode {
    /// The script as the model wrote it, its `// @options:` line included.
    pub code: String,
    pub state: CallState,
    /// A status for the head after the calls, such as `interrupted`.
    pub summary: Option<String>,
    /// The calls the script made, in the order it made them.
    pub calls: Vec<NestedCall>,
    /// What went back to the model, without the result's header; empty
    /// while the script runs.
    pub output: Vec<String>,
    /// The file holding the whole output, when it was cut.
    pub full_output_path: Option<String>,
    /// How long the script ran, from the result's header.
    pub wall_time: Option<Duration>,
}

/// How long the script ran, when `text` is the header a codemode result
/// opens with, Pi's `SCRIPT_HEADER`:
/// `Script completed|failed\nWall time N seconds\nOutput:\n`. A result
/// without it, such as rejected options, has `None`.
pub fn wall_time(text: &str) -> Option<Duration> {
    let rest = text
        .strip_prefix("Script completed\n")
        .or_else(|| text.strip_prefix("Script failed\n"))?;
    let (seconds, rest) = rest.strip_prefix("Wall time ")?.split_once(" seconds\n")?;
    if rest != "Output:\n" || !seconds.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Duration::try_from_secs_f64(seconds.parse().ok()?).ok()
}

/// A call's duration, Pi's renderer `formatDuration`: `5ms` under a second,
/// `1.2s` from one.
pub fn call_duration(duration: Duration) -> String {
    let ms = duration.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{}ms", ms.round())
    } else {
        format!("{}s", fixed1(ms / 1000.0))
    }
}

/// `value` with one decimal, a tie rounded up as JavaScript's `toFixed(1)`
/// rounds it.
fn fixed1(value: f64) -> String {
    format!("{:.1}", (value * 10.0).round() / 10.0)
}

/// How long a script took, Pi's `formatToolDuration`: `5.1s`, `2m 5s`, or
/// `1h 2m 5s`.
pub fn took(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 60.0 {
        return format!("{}s", fixed1(seconds));
    }
    let total = duration.as_secs();
    let (minutes, remainder) = (total / 60, total % 60);
    if minutes < 60 {
        format!("{minutes}m {remainder}s")
    } else {
        format!("{}h {}m {remainder}s", minutes / 60, minutes % 60)
    }
}

/// `text` without escape sequences or control characters, its tabs as
/// spaces, so it cannot change the terminal or the cells it is drawn in.
fn clean(text: &str) -> String {
    shell_output::parse_sgr(text, Tones::None).0
}

/// The lines of `text`, each cleaned.
fn clean_lines(text: &str) -> Vec<String> {
    text.lines().map(clean).collect()
}

/// `lines` without the blank lines at either end.
fn trimmed(lines: &[String]) -> &[String] {
    let blank = |line: &String| line.trim().is_empty();
    let start = lines
        .iter()
        .position(|line| !blank(line))
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !blank(line))
        .map_or(start, |i| i + 1);
    &lines[start..end]
}

/// The block's rows: its head, the script, the calls, and the output.
pub(crate) fn present(script: &Codemode, width: usize, look: Look) -> Vec<Line<'static>> {
    let quiet = if script.state == CallState::Failed {
        transcript::red(look)
    } else {
        transcript::dim()
    };
    let glyph = transcript::gutter(look);
    let gutter = || {
        vec![
            transcript::pad(RAIL),
            Span::styled(format!("{glyph} "), quiet),
        ]
    };
    let mut lines = head(script, width, look);
    lines.extend(source(&script.code, width, look, &gutter));
    let calls = calls(&script.calls, width, look, &gutter);
    if !calls.is_empty() {
        lines.push(Line::from(gutter()));
        lines.extend(calls);
    }
    // Pi draws no output while the result is partial.
    if script.state != CallState::Running {
        let output = output(script, width, look, &gutter);
        if !output.is_empty() {
            lines.push(Line::from(gutter()));
            lines.extend(output);
        }
    }
    lines
}

/// `{} codemode:`, labelled as every call is by its tool, then its status.
fn head(script: &Codemode, width: usize, look: Look) -> Vec<Line<'static>> {
    let (mark, style) = transcript::script_mark(script.state, look);
    let mut lines = vec![Line::from(vec![
        transcript::pad(RAIL),
        Span::styled(mark, style),
        transcript::pad(1),
        Span::styled(copy::CODEMODE, Style::new().add_modifier(Modifier::BOLD)),
    ])];
    let under = vec![transcript::pad(RAIL + mark.width() + 1)];
    transcript::ride(&mut lines, status(script, look), under, width);
    lines
}

/// The head's status: how many calls the script made; while it runs, how
/// many are running; once it ended, how many failed or were cancelled and
/// how long it took; then its own status, such as an interruption.
fn status(script: &Codemode, look: Look) -> Vec<Span<'static>> {
    let mut parts: Vec<Span<'static>> = Vec::new();
    let mut push = |text: String, style: Style| {
        if !parts.is_empty() {
            parts.push(Span::styled(copy::STATUS_SEP, transcript::dim()));
        }
        parts.push(Span::styled(text, style));
    };
    let calls = &script.calls;
    let count = |status| calls.iter().filter(|call| call.status == status).count();
    if !calls.is_empty() {
        let noun = if calls.len() == 1 {
            copy::CALL
        } else {
            copy::CALLS
        };
        push(format!("{} {noun}", calls.len()), transcript::dim());
    }
    if script.state == CallState::Running {
        let running = count(NestedStatus::Running);
        if running > 0 {
            push(format!("{running} {}", copy::RUNNING), transcript::dim());
        }
    } else {
        for (status, word, style) in [
            (NestedStatus::Error, copy::FAILED, transcript::red(look)),
            (
                NestedStatus::Cancelled,
                copy::CANCELLED,
                transcript::yellow(look),
            ),
        ] {
            let n = count(status);
            if n > 0 {
                push(format!("{n} {word}"), style);
            }
        }
    }
    if let Some(wall_time) = script.wall_time {
        push(
            format!("{} {}", copy::TOOK, took(wall_time)),
            transcript::dim(),
        );
    }
    if let Some(summary) = &script.summary {
        if !parts.is_empty() {
            parts.push(transcript::pad(2));
        }
        parts.extend(transcript::status_spans(
            summary,
            transcript::red(look),
            look,
        ));
    }
    parts
}

/// The first `limit` rows `build` makes of `lines`, and a count of the
/// rest on a row opening with `lead`, as Pi's `VisualLinePreview` keeps a
/// start: it counts wrapped rows, not lines, since one long line can fill a
/// screen. A count that would stand for one row is that row, as in the
/// transcript's other previews.
///
/// `build` makes the rows of line `i`, wrapped at `width` less `indent`, and
/// is called in order, so a lexer it carries sees every line before the
/// one it draws. Once the rows run past what is shown it is not called
/// again: the lines after are only measured, so a long script or output
/// costs its count, not its drawing.
fn keep(
    lines: &[String],
    limit: usize,
    width: usize,
    indent: usize,
    look: Look,
    lead: &dyn Fn() -> Vec<Span<'static>>,
    mut build: impl FnMut(usize, &str) -> Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut built = 0;
    while built < lines.len() && rows.len() <= limit + 1 {
        rows.extend(build(built, &lines[built]));
        built += 1;
    }
    let total = rows.len()
        + lines[built..]
            .iter()
            .map(|line| transcript::hang_rows(line, width, indent))
            .sum::<usize>();
    if total > limit + 1 {
        rows.truncate(limit);
        let mut spans = lead();
        spans.push(Span::styled(
            format!(
                "{} {} {}",
                look.marks().more,
                total - limit,
                copy::MORE_LINES
            ),
            transcript::dim(),
        ));
        rows.push(Line::from(spans));
    }
    rows
}

/// The script, numbered and coloured, its first [`CODE_PREVIEW_LINES`]
/// rows.
fn source(
    code: &str,
    width: usize,
    look: Look,
    gutter: &dyn Fn() -> Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let lines = clean_lines(code.trim_end());
    if lines.is_empty() {
        return Vec::new();
    }
    let digits = lines.len().to_string().len();
    // Code starts after the gutter, the number, and two cells.
    let column = BODY + digits + 2;
    // Lexed in order, every line drawn, so a comment opened on one line
    // still colours the lines after it.
    let mut carry = Carry::default();
    keep(
        &lines,
        CODE_PREVIEW_LINES,
        width,
        column,
        look,
        gutter,
        |i, line| {
            let runs = transcript::code_runs(line, Some(Lang::TypeScript), &mut carry, look.tones);
            let mut lead = gutter();
            lead.push(Span::styled(
                format!("{:>digits$}  ", i + 1),
                transcript::dim(),
            ));
            let mut rest = gutter();
            rest.push(transcript::pad(digits + 2));
            transcript::hang_runs(line, &runs, Style::new(), width, column, lead, rest)
        },
    )
}

/// A call's mark: a running dot that blinks, a check, a cross, or the
/// cancelled mark in the interruption's yellow.
fn nested_mark(status: NestedStatus, look: Look) -> (&'static str, Style) {
    match status {
        NestedStatus::Running => transcript::mark_of(CallState::Running, look),
        NestedStatus::Ok => transcript::mark_of(CallState::Done, look),
        NestedStatus::Error => transcript::mark_of(CallState::Failed, look),
        NestedStatus::Cancelled => (look.marks().cancelled, transcript::yellow(look)),
    }
}

/// A call's arguments as its row shows them, cut at
/// [`COLLAPSED_ARGS_CHARS`] as Pi's collapsed row cuts them.
fn args_preview(args: &str) -> String {
    let args = clean(args);
    if args.chars().count() <= COLLAPSED_ARGS_CHARS {
        return args;
    }
    let kept: String = args.chars().take(COLLAPSED_ARGS_CHARS - 3).collect();
    format!("{kept}...")
}

/// `text` on the row `first` opens, after its cells, then wrapped at
/// `width` less `indent` on rows opening with `rest`; so a call's arguments
/// go on beside a name that runs past the shared column, and keep the
/// column's room on the rows after.
fn hang_beside(
    text: &str,
    width: usize,
    indent: usize,
    mut first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    style: Style,
) -> Vec<Line<'static>> {
    let used: usize = first.iter().map(Span::width).sum();
    let room = width.saturating_sub(used);
    let end = if room == 0 {
        0
    } else {
        editor::layout(text, 0, room + 1)
            .rows
            .first()
            .map_or(0, |row| row.end)
    };
    let (beside, after) = (text[..end].trim_end(), text[end..].trim_start());
    if !beside.is_empty() {
        first.push(Span::styled(beside.to_owned(), style));
    }
    let mut lines = vec![Line::from(first)];
    if !after.is_empty() {
        lines.extend(transcript::hang(
            after,
            width,
            indent,
            rest.clone(),
            rest,
            style,
        ));
    }
    lines
}

/// The latest [`CALL_PREVIEW_COUNT`] calls, one row each: its mark, its
/// name, its arguments, and its duration, and a failed or cancelled call's
/// first [`ERROR_PREVIEW_ROWS`] rows of error under its name. Earlier calls
/// are counted above them.
fn calls(
    calls: &[NestedCall],
    width: usize,
    look: Look,
    gutter: &dyn Fn() -> Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    // A count that would stand for one call is that call.
    let shown = if calls.len() > CALL_PREVIEW_COUNT + 1 {
        &calls[calls.len() - CALL_PREVIEW_COUNT..]
    } else {
        calls
    };
    let hidden = calls.len() - shown.len();
    let mut rows = Vec::new();
    if hidden > 0 {
        let mut spans = gutter();
        spans.push(Span::styled(
            format!("{} {hidden} {}", look.marks().more, copy::EARLIER_CALLS),
            transcript::dim(),
        ));
        rows.push(Line::from(spans));
    }
    let names: Vec<String> = shown.iter().map(|call| clean(&call.name)).collect();
    // The arguments of every row start in one column, at most a third of
    // the width wide, so one long name such as an MCP tool's leaves the
    // other rows their room. A name past it takes one space, as Pi's
    // `name args`.
    let longest = names.iter().map(|name| name.width()).max().unwrap_or(0);
    let column = (longest + 2).min(width / 3);
    for (call, name) in shown.iter().zip(names) {
        let (mark, mark_style) = nested_mark(call.status, look);
        let mut first = gutter();
        first.extend([Span::styled(mark, mark_style), transcript::pad(1)]);
        let name_at = BODY + mark.width() + 1;
        let indent = name_at + column;
        let gap = column.saturating_sub(name.width()).max(1);
        first.push(Span::styled(
            name.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        first.push(transcript::pad(gap));
        let mut rest = gutter();
        rest.push(transcript::pad(indent - BODY));
        let mut row = hang_beside(
            &args_preview(&call.args),
            width,
            indent,
            first,
            rest.clone(),
            transcript::dim(),
        );
        if let Some(duration) = call.duration {
            transcript::ride(
                &mut row,
                vec![Span::styled(call_duration(duration), transcript::dim())],
                rest,
                width,
            );
        }
        rows.extend(row);
        let Some(error) = &call.error else {
            continue;
        };
        let style = if call.status == NestedStatus::Cancelled {
            transcript::yellow(look)
        } else {
            transcript::red(look)
        };
        let mut under = gutter();
        under.push(transcript::pad(name_at - BODY));
        let lead = || under.clone();
        rows.extend(keep(
            trimmed(&clean_lines(error)),
            ERROR_PREVIEW_ROWS,
            width,
            name_at,
            look,
            &lead,
            |_, text| transcript::hang(text, width, name_at, lead(), lead(), style),
        ));
    }
    rows
}

/// What went back to the model: its first [`OUTPUT_PREVIEW_LINES`] rows, in
/// red when the script failed, then the file holding all of it when it was
/// cut.
fn output(
    script: &Codemode,
    width: usize,
    look: Look,
    gutter: &dyn Fn() -> Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let lines: Vec<String> = script.output.iter().map(|line| clean(line)).collect();
    let lines = trimmed(&lines);
    if lines.is_empty() {
        return Vec::new();
    }
    let style = if script.state == CallState::Failed {
        transcript::red(look)
    } else {
        transcript::output_tone(look.tones)
    };
    let mut rows = keep(
        lines,
        OUTPUT_PREVIEW_LINES,
        width,
        BODY,
        look,
        gutter,
        |_, line| transcript::hang(line, width, BODY, gutter(), gutter(), style),
    );
    if let Some(path) = &script.full_output_path {
        rows.extend(transcript::hang(
            &format!("{} {}", copy::FULL_OUTPUT, clean(path)),
            width,
            BODY,
            gutter(),
            gutter(),
            transcript::dim(),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{Row, present as present_rows};

    const PLAIN: Look = Look {
        tones: Tones::None,
        classic: false,
        lit: true,
    };

    const HEADER: &str = "Script completed\nWall time 0.1 seconds\nOutput:\n";

    fn script(state: CallState) -> Codemode {
        Codemode {
            code: String::new(),
            state,
            summary: None,
            calls: Vec::new(),
            output: Vec::new(),
            full_output_path: None,
            wall_time: None,
        }
    }

    fn call(name: &str, args: &str, status: NestedStatus, ms: Option<u64>) -> NestedCall {
        NestedCall {
            name: name.into(),
            args: args.into(),
            status,
            duration: ms.map(Duration::from_millis),
            error: None,
        }
    }

    fn render(script: Codemode, width: usize, look: Look) -> Vec<String> {
        present_rows(&[Row::Codemode(script)], 0, width, look)
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    }

    // Pi: "hides the script header and shows the output".
    #[test]
    fn hides_the_script_header_and_shows_the_output() {
        assert_eq!(wall_time(HEADER), Some(Duration::from_millis(100)));
        assert_eq!(
            wall_time("Script failed\nWall time 12.5 seconds\nOutput:\n"),
            Some(Duration::from_millis(12_500))
        );
        let mut done = script(CallState::Done);
        done.calls = vec![call("read", r#"{"path":"a"}"#, NestedStatus::Ok, Some(5))];
        done.output = vec!["hello".into()];
        done.wall_time = wall_time(HEADER);
        assert_eq!(
            render(done, 60, PLAIN),
            [
                "  ✓ codemode:  1 call · took 0.1s",
                "  │",
                r#"  │ ✓ read  {"path":"a"}  5ms"#,
                "  │",
                "  │ hello",
            ]
        );
    }

    // Pi's "shows the cost of model calls and their total" is not ported:
    // Bake's code mode has no `models` (D42), so no call has a cost.

    // Pi: "shows results without a header, such as rejected options".
    #[test]
    fn shows_results_without_a_header_such_as_rejected_options() {
        let text = "The @options line must be followed by JavaScript source";
        assert_eq!(wall_time(text), None);
        let mut rejected = script(CallState::Failed);
        rejected.code = "// @options: {}".into();
        rejected.output = vec![text.into()];
        assert_eq!(
            render(rejected, 80, PLAIN),
            [
                "  ✗ codemode:",
                "  │ 1  // @options: {}",
                "  │",
                "  │ The @options line must be followed by JavaScript source",
            ]
        );
    }

    // Pi: "limits collapsed output to wrapped lines, not logical lines".
    #[test]
    fn limits_collapsed_output_to_wrapped_lines_not_logical_lines() {
        let mut long = script(CallState::Done);
        long.output = vec!["x".repeat(1000)];
        long.full_output_path = Some("/tmp/out.txt".into());
        // Output starts at the body column, so 54 cells leave it Pi's 50.
        let lines = render(long, BODY + 50, PLAIN);
        assert_eq!(lines.len(), 1 + 1 + 7);
        let output = &lines[2..];
        assert!(
            output[..5]
                .iter()
                .all(|line| *line == format!("  │ {}", "x".repeat(50)))
        );
        assert_eq!(output[5], "  │ ⋯ 15 more lines");
        assert_eq!(output[6], "  │ Full output: /tmp/out.txt");
    }

    // Pi: "shows the elapsed time while the script runs and the total once
    // it finished" (#10708). The view keeps no clock, so a running block
    // says what runs, and a finished one how long the script took.
    #[test]
    fn shows_what_runs_while_the_script_runs_and_the_total_once_it_finished() {
        let mut running = script(CallState::Running);
        running.calls = vec![call(
            "bash",
            r#"{"command":"sleep 3"}"#,
            NestedStatus::Running,
            None,
        )];
        let lines = render(running.clone(), 60, PLAIN);
        assert_eq!(lines[0], "  ● codemode:  1 call · 1 running");
        assert_eq!(lines[2], r#"  │ ● bash  {"command":"sleep 3"}"#);
        let mut done = running;
        done.state = CallState::Done;
        done.calls[0].status = NestedStatus::Ok;
        done.calls[0].duration = Some(Duration::from_millis(3_004));
        done.wall_time = Some(Duration::from_millis(5_100));
        let lines = render(done, 60, PLAIN);
        assert_eq!(lines[0], "  ✓ codemode:  1 call · took 5.1s");
        assert_eq!(lines[2], r#"  │ ✓ bash  {"command":"sleep 3"}  3.0s"#);
        assert!(lines.iter().all(|line| !line.contains("running")));
    }

    #[test]
    fn a_long_script_keeps_its_first_rows_and_counts_the_rest() {
        let mut long = script(CallState::Done);
        long.code = (1..=12)
            .map(|n| format!("step{n}();"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = render(long, 40, PLAIN);
        assert_eq!(lines.len(), 1 + CODE_PREVIEW_LINES + 1);
        assert_eq!(lines[1], "  │  1  step1();");
        assert_eq!(lines[10], "  │ 10  step10();");
        assert_eq!(lines[11], "  │ ⋯ 2 more lines");
    }

    #[test]
    fn rows_show_the_latest_calls_with_errors_and_cancellations() {
        let mut ran = script(CallState::Failed);
        ran.calls = (1..=10)
            .map(|n| {
                call(
                    "read",
                    &format!(r#"{{"path":"f{n}"}}"#),
                    NestedStatus::Ok,
                    Some(n),
                )
            })
            .collect();
        ran.calls[8] = NestedCall {
            error: Some("ENOENT: no such file\nat read".into()),
            ..call("read", r#"{"path":"f9"}"#, NestedStatus::Error, Some(2))
        };
        ran.calls[9] = NestedCall {
            error: Some("This operation was aborted".into()),
            ..call(
                "bash",
                &format!(r#"{{"command":"{}"}}"#, "y".repeat(90)),
                NestedStatus::Cancelled,
                Some(1_500),
            )
        };
        let lines = render(ran, 120, PLAIN);
        assert_eq!(
            lines[..4],
            [
                "  ✗ codemode:  10 calls · 1 failed · 1 cancelled",
                "  │",
                "  │ ⋯ 2 earlier calls",
                r#"  │ ✓ read  {"path":"f3"}  3ms"#,
            ]
        );
        assert_eq!(
            lines[9..],
            [
                r#"  │ ✗ read  {"path":"f9"}  2ms"#,
                "  │   ENOENT: no such file",
                "  │   at read",
                format!(
                    r#"  │ ⊘ bash  {{"command":"{}...  1.5s"#,
                    "y".repeat(COLLAPSED_ARGS_CHARS - 3 - 12)
                )
                .as_str(),
                "  │   This operation was aborted",
            ]
        );
        let classic = render(
            Codemode {
                calls: vec![call("bash", "{}", NestedStatus::Cancelled, None)],
                ..script(CallState::Failed)
            },
            40,
            Look {
                classic: true,
                ..PLAIN
            },
        );
        assert_eq!(classic[2], "  | / bash  {}");
    }

    #[test]
    fn a_failed_calls_error_keeps_its_first_rows_on_a_narrow_terminal() {
        let mut failed = script(CallState::Failed);
        failed.calls = (0..CALL_PREVIEW_COUNT)
            .map(|_| NestedCall {
                error: Some(format!("{}\n\nat run", "x".repeat(500))),
                ..call("bash", "{}", NestedStatus::Error, None)
            })
            .collect();
        let lines = render(failed, 40, PLAIN);
        // Errors start under the name, 34 cells wide: the 500 characters
        // wrap into 15 rows, and the blank and the stack line add two.
        assert_eq!(
            lines[2..6],
            [
                "  │ ✗ bash  {}".to_owned(),
                format!("  │   {}", "x".repeat(34)),
                format!("  │   {}", "x".repeat(34)),
                "  │   ⋯ 15 more lines".to_owned(),
            ]
        );
        // Each call takes its row, two of error, and the count.
        assert_eq!(lines.len(), 2 + CALL_PREVIEW_COUNT * 4);
        // A count that would stand for one row is that row.
        let mut short = script(CallState::Failed);
        short.calls = vec![NestedCall {
            error: Some("one\ntwo\nthree".into()),
            ..call("bash", "{}", NestedStatus::Error, None)
        }];
        assert_eq!(
            render(short, 40, PLAIN)[3..],
            ["  │   one", "  │   two", "  │   three"]
        );
    }

    #[test]
    fn a_long_name_takes_one_space_and_leaves_the_other_rows_their_column() {
        let mut ran = script(CallState::Done);
        ran.calls = vec![
            call("read", r#"{"path":"a"}"#, NestedStatus::Ok, Some(5)),
            call(
                "mcp__github__list_issues",
                r#"{"repo":"bake","state":"open"}"#,
                NestedStatus::Ok,
                Some(12),
            ),
        ];
        let lines = render(ran, 40, PLAIN);
        // The column is a third of 40 cells, so `read` keeps one row.
        assert_eq!(lines[2], r#"  │ ✓ read         {"path":"a"}  5ms"#);
        // The long name takes one space, its arguments go on beside it, and
        // the rows after keep the column.
        assert_eq!(
            lines[3..],
            [
                r#"  │ ✓ mcp__github__list_issues {"repo":""#,
                r#"  │                bake","state":"open"}"#,
                "  │                12ms",
            ]
        );
        // A name wider than the row puts its arguments under it.
        let mut wide = script(CallState::Done);
        wide.calls = vec![call(&"n".repeat(50), "{}", NestedStatus::Ok, None)];
        let lines = render(wide, 40, PLAIN);
        assert_eq!(
            lines[2..],
            [
                format!("  │ ✓ {}", "n".repeat(50)),
                "  │                {}".to_owned()
            ]
        );
    }

    #[test]
    fn a_long_script_and_output_draw_only_what_they_show() {
        let mut long = script(CallState::Done);
        long.code = std::iter::once("/* opened".to_owned())
            .chain((2..=10_000).map(|n| format!("step{n}();")))
            .collect::<Vec<_>>()
            .join("\n");
        // Each output line wraps into two rows of 36 cells.
        long.output = (1..=10_000).map(|n| format!("{n:0>40}")).collect();
        long.full_output_path = Some("/tmp/out.txt".into());
        let lines = render(long.clone(), 40, PLAIN);
        assert_eq!(lines.len(), 1 + 11 + 1 + 6 + 1);
        assert_eq!(lines[1], "  │     1  /* opened");
        assert_eq!(lines[10], "  │    10  step10();");
        assert_eq!(lines[11], "  │ ⋯ 9990 more lines");
        assert_eq!(lines[13], format!("  │ {}", "0".repeat(36)));
        assert_eq!(lines[14], "  │ 0001");
        assert_eq!(lines[16], "  │ 0002");
        assert_eq!(lines[18], "  │ ⋯ 19995 more lines");
        assert_eq!(lines[19], "  │ Full output: /tmp/out.txt");
        // Lexed in order: the comment the first line opens colours the next.
        let colour = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let drawn = present_rows(&[Row::Codemode(long)], 0, 40, colour);
        let style = |row: usize| drawn[row].spans.last().map(|span| span.style);
        assert_eq!(style(2), style(1));
        // Lines past the shown rows are measured, never drawn.
        let lines: Vec<String> = (0..10_000).map(|n| n.to_string()).collect();
        let mut drawn = 0;
        let rows = keep(&lines, 5, 40, BODY, PLAIN, &Vec::new, |_, line| {
            drawn += 1;
            vec![Line::raw(line.to_owned())]
        });
        assert_eq!(drawn, 7);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[5].to_string(), "⋯ 9995 more lines");
    }

    #[test]
    fn the_head_takes_braces_on_colour_and_an_interruption_as_a_tag() {
        let look = Look {
            tones: Tones::TrueColor,
            ..PLAIN
        };
        let mut stopped = script(CallState::Failed);
        stopped.summary = Some(copy::INTERRUPTED_NOTE.into());
        stopped.calls = vec![call("bash", "{}", NestedStatus::Cancelled, None)];
        let lines = render(stopped, 60, look);
        assert_eq!(
            lines[1],
            "  {} codemode:  1 call · 1 cancelled   interrupted"
        );
        // A blinking head keeps its width while hidden.
        let mut running = script(CallState::Running);
        running.code = "return 1;".into();
        let on = render(running.clone(), 40, look);
        let off = render(running, 40, Look { lit: false, ..look });
        assert_eq!(on[1], "  {} codemode:");
        assert_eq!(off[1], "     codemode:");
    }

    #[test]
    fn escapes_never_reach_the_screen() {
        let mut dirty = script(CallState::Done);
        dirty.code = "text(\"\u{1b}[31mred\")".into();
        dirty.calls = vec![NestedCall {
            error: Some("\u{1b}]0;title\u{7}bad".into()),
            ..call("re\u{1b}[2Jad", "{\u{7}}", NestedStatus::Error, None)
        }];
        dirty.output = vec!["\u{1b}[1mbold\u{1b}[0m".into()];
        let lines = render(dirty, 60, PLAIN);
        assert!(
            lines.iter().all(|line| !line.chars().any(char::is_control)),
            "{lines:?}"
        );
        assert_eq!(lines[1], "  │ 1  text(\"red\")");
        assert_eq!(lines[3], "  │ ✗ read  {}");
        assert_eq!(lines[4], "  │   bad");
        assert_eq!(lines[6], "  │ bold");
    }

    #[test]
    fn durations_read_as_pi_writes_them() {
        assert_eq!(call_duration(Duration::from_micros(4_600)), "5ms");
        assert_eq!(call_duration(Duration::from_millis(999)), "999ms");
        assert_eq!(call_duration(Duration::from_millis(1_250)), "1.3s");
        assert_eq!(took(Duration::from_millis(600)), "0.6s");
        assert_eq!(took(Duration::from_secs(125)), "2m 5s");
        assert_eq!(took(Duration::from_secs(3_725)), "1h 2m 5s");
    }
}
