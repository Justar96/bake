//! Background shells: the session's shells as the runtime reports them,
//! and one shell's output. The row under the composer and the list that
//! show them beside the subagents are [`crate::tasks`]'s.
//!
//! The runtime reports every job its session retains
//! ([`crate::runtime::RuntimeUpdate::Jobs`]), in start order, each with its
//! newest output. Jobs belong to the process: a new session, or a restart,
//! starts with none, so nothing here outlives the runtime that reported
//! it. The view draws what was reported and asks the runtime to stop a job
//! ([`crate::state::Effect::StopJob`]); the runtime reports the stop.

use std::time::Duration;

use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity;
use crate::copy;
use crate::shell_output;
use crate::tasks;
use crate::transcript::{self, Look};

/// One background shell as the runtime reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundJob {
    /// Its id, such as `bash-1`.
    pub id: String,
    /// The command it runs.
    pub command: String,
    pub state: JobState,
    /// How long it had run when reported, or ran in all once finished. A
    /// running job's clock goes on from here ([`elapsed`]).
    pub elapsed: Duration,
    /// Its newest output, read by the model or not.
    pub output: String,
    /// Bytes of output before `output` that are no longer kept.
    pub omitted_bytes: u64,
}

/// Where a job is in its life, with the runtime's detail once it ended,
/// such as `exit code: 0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    /// A stop was asked for; the command has not ended yet.
    Stopping,
    Completed(Option<String>),
    Killed(Option<String>),
    Failed(Option<String>),
}

impl JobState {
    /// Whether the job still runs, a requested stop included.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }

    /// Whether the job ended badly: an exit other than 0, or a failure to run.
    pub fn failed(&self) -> bool {
        match self {
            Self::Completed(detail) => exit_code(detail.as_deref()).is_some_and(|code| code != 0),
            Self::Failed(_) => true,
            _ => false,
        }
    }
}

/// How long `job` has run at `now`, its report having arrived at `at`.
pub fn elapsed(job: &BackgroundJob, at: Duration, now: Duration) -> Duration {
    if job.state.is_live() {
        job.elapsed + now.saturating_sub(at)
    } else {
        job.elapsed
    }
}

/// The exit code a completed job's detail names, `exit code: N`.
fn exit_code(detail: Option<&str>) -> Option<i64> {
    detail?.strip_prefix("exit code: ")?.trim().parse().ok()
}

/// The word for how the job is doing: `running`, `stopping`, `exit 0`,
/// `stopped`, or `failed`.
pub fn word(state: &JobState) -> String {
    match state {
        JobState::Running => copy::JOB_RUNNING.to_owned(),
        JobState::Stopping => copy::JOB_STOPPING.to_owned(),
        JobState::Completed(detail) => match exit_code(detail.as_deref()) {
            Some(code) => format!("{} {code}", copy::JOB_EXIT),
            None => copy::JOB_DONE.to_owned(),
        },
        JobState::Killed(_) => copy::JOB_STOPPED.to_owned(),
        JobState::Failed(_) => copy::JOB_FAILED.to_owned(),
    }
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// One shell: its command, then its state as its row in the tasks list
/// shows it (its mark and id, and, at the right, its state word and time),
/// a rule, then its newest output in at most `height` rows, `scroll` rows
/// up from its end. Returns the rows and the most `scroll` can be.
pub fn shell(
    job: &BackgroundJob,
    at: Duration,
    now: Duration,
    width: usize,
    height: usize,
    scroll: usize,
    look: Look,
) -> (Vec<Line<'static>>, usize) {
    // A long command keeps its first rows, the rest counted at the right of
    // the last, so the output keeps the panel.
    // The model writes the command: its escape sequences and other control
    // characters are taken out, line by line.
    let command: Vec<String> = job
        .command
        .trim_end()
        .split('\n')
        .map(|line| shell_output::parse_sgr(line, activity::Tones::None).0)
        .collect();
    let mut command = transcript::wrap(&command.join("\n"), width.saturating_sub(4));
    let hidden = command.len().saturating_sub(COMMAND_ROWS);
    command.truncate(COMMAND_ROWS);
    let rows = command.len();
    let mut lines: Vec<Line<'static>> = command
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            let mut spans = vec![
                Span::raw(if i == 0 { "  $ " } else { "    " }),
                Span::styled(part, Style::new().add_modifier(Modifier::BOLD)),
            ];
            if hidden > 0 && i + 1 == rows {
                let count = crate::cards::count(hidden, copy::LINE_NOUNS);
                spans.push(Span::styled(format!("  +{count}"), dim()));
            }
            Line::from(spans)
        })
        .collect();
    lines.push(state_row(job, at, now, width, look));
    let family = shell_output::family(&job.command);
    let mut output: Vec<Line<'static>> = Vec::new();
    for line in job.output.trim_end_matches('\n').split('\n') {
        let styled = shell_output::styled(line, family, look.tones);
        output.extend(transcript::hang_runs(
            &styled.text.replace('\t', "    "),
            &styled.runs,
            transcript::output_tone(look.tones),
            width,
            2,
            vec![Span::raw("  ")],
            vec![Span::raw("  ")],
        ));
    }
    if job.output.is_empty() {
        output = vec![Line::styled(format!("  {}", copy::JOB_NO_OUTPUT), dim())];
    }
    // The rule under the header counts the output rows out of view, above
    // and below, as the composer's edges count its draft's, after the size
    // of the output no longer kept.
    let room = height.saturating_sub(lines.len() + 1);
    let most = output.len().saturating_sub(room);
    let scroll = scroll.min(most);
    let end = output.len() - scroll;
    let start = end.saturating_sub(room);
    let mut counts = Vec::new();
    if job.omitted_bytes > 0 {
        counts.push(format!(
            "{} {}",
            copy::JOB_EARLIER,
            bytes(job.omitted_bytes)
        ));
    }
    if start > 0 {
        counts.push(format!("+{start} {}", copy::ABOVE));
    }
    if scroll > 0 {
        counts.push(format!("+{scroll} {}", copy::BELOW));
    }
    lines.push(counted_rule(width, &counts.join(copy::STATUS_SEP), look));
    lines.extend(output.drain(start..end));
    (lines, most)
}

/// The shell's state as its row in the tasks list draws it: its mark and
/// id, and its state word and time at the right, ending where the list's
/// rows do. Short of room, the time, then the word give way whole.
fn state_row(
    job: &BackgroundJob,
    at: Duration,
    now: Duration,
    width: usize,
    look: Look,
) -> Line<'static> {
    let end = width.saturating_sub(transcript::RAIL);
    let lead = vec![
        Span::raw("  "),
        tasks::mark(tasks::shell_phase(&job.state), look),
        Span::raw(" "),
        Span::raw(tasks::one_line(&job.id)),
    ];
    let left = Line::from(lead.clone()).width();
    let word = word(&job.state);
    let time = activity::format_elapsed(elapsed(job, at, now));
    for right in [format!("{word}  {time}"), word] {
        if left + 2 + right.width() <= end {
            let mut spans = lead;
            spans.push(Span::raw(" ".repeat(end - left - right.width())));
            spans.push(Span::styled(right, dim()));
            return Line::from(spans);
        }
    }
    Line::from(lead)
}

/// Command rows the shell view draws before it counts the rest.
pub const COMMAND_ROWS: usize = 3;

/// The transcript's rule across `width`, `label` near its right end with a
/// rule cell after it, or the plain rule where the label does not fit.
pub(crate) fn counted_rule(width: usize, label: &str, look: Look) -> Line<'static> {
    let rule = transcript::rule(width, transcript::RAIL, look);
    let label = format!(" {label} ");
    let cells = width.saturating_sub(2 * transcript::RAIL);
    if label.trim().is_empty() || label.chars().count() + 2 > cells {
        return rule;
    }
    let style = rule.spans.last().map_or_else(dim, |span| span.style);
    let glyph = look.marks().rule;
    let before = cells - label.chars().count() - 1;
    Line::from(vec![
        transcript::pad(transcript::RAIL),
        Span::styled(glyph.repeat(before), style),
        Span::styled(label, dim()),
        Span::styled(glyph.to_owned(), style),
    ])
}

/// A byte count as the shell view words it: `512 B`, `12.4 KB`, `3.1 MB`.
fn bytes(count: u64) -> String {
    match count {
        0..1_000 => format!("{count} B"),
        1_000..1_000_000 => format!("{:.1} KB", count as f64 / 1_000.0),
        _ => format!("{:.1} MB", count as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Tones;

    fn job(id: &str, state: JobState) -> BackgroundJob {
        BackgroundJob {
            id: id.into(),
            command: format!("run {id}"),
            state,
            elapsed: Duration::from_secs(5),
            output: String::new(),
            omitted_bytes: 0,
        }
    }

    fn look() -> Look {
        Look {
            tones: Tones::None,
            classic: false,
            lit: true,
            mermaid: crate::mermaid::Mermaid::Streaming,
        }
    }

    fn text(line: &Line) -> String {
        line.to_string()
    }

    #[test]
    fn a_running_clock_goes_on_from_the_report_and_a_finished_one_stops() {
        let running = job("bash-1", JobState::Running);
        let done = job("bash-2", JobState::Completed(Some("exit code: 0".into())));
        let (at, now) = (Duration::from_secs(10), Duration::from_secs(13));
        assert_eq!(elapsed(&running, at, now), Duration::from_secs(8));
        assert_eq!(elapsed(&done, at, now), Duration::from_secs(5));
    }

    #[test]
    fn a_command_with_control_characters_lays_out_without_panicking() {
        // The model writes the command, so it may hold any character.
        let mut job = job("bash-1", JobState::Running);
        job.command = "printf 'a\rb'\r\x07 && echo\x1b".into();
        let at = Duration::ZERO;
        for width in 0..=16 {
            let (lines, _) = shell(&job, at, at, width, 8, 0, look());
            for line in &lines {
                let shown = text(line);
                assert!(!shown.chars().any(char::is_control), "{shown:?}");
            }
        }
        let (lines, _) = shell(&job, at, at, 12, 8, 0, look());
        assert!(text(&lines[0]).contains("printf"));
    }

    #[test]
    fn words_name_the_exit_code_and_the_stop() {
        assert_eq!(word(&JobState::Running), "running");
        assert_eq!(
            word(&JobState::Completed(Some("exit code: 2".into()))),
            "exit 2"
        );
        assert_eq!(word(&JobState::Completed(None)), "done");
        assert_eq!(
            word(&JobState::Killed(Some("killed before exit".into()))),
            "stopped"
        );
        assert_eq!(word(&JobState::Failed(Some("spawn".into()))), "failed");
    }

    // Bake: a long command keeps its first rows with a count of the rest,
    // and the rule counts the output out of view above and below, as the
    // composer's edges do.
    #[test]
    fn a_long_command_is_cut_and_the_rule_counts_hidden_output() {
        let mut long = job("bash-1", JobState::Running);
        long.command = (0..30).map(|n| format!("echo {n}\n")).collect();
        long.output = (1..=50).map(|n| format!("line {n}\n")).collect();
        let (lines, most) = shell(&long, Duration::ZERO, Duration::ZERO, 40, 12, 0, look());
        assert_eq!(lines.len(), 12);
        assert!(
            text(&lines[COMMAND_ROWS - 1]).ends_with(&format!("+{} lines", 30 - COMMAND_ROWS)),
            "{}",
            text(&lines[COMMAND_ROWS - 1])
        );
        let rule = text(&lines[COMMAND_ROWS + 1]);
        assert!(rule.contains(" +43 above "), "{rule}");
        assert!(!rule.contains("below"), "{rule}");
        assert_eq!(text(&lines[11]), "  line 50");
        assert_eq!(most, 43);
        let (lines, _) = shell(&long, Duration::ZERO, Duration::ZERO, 40, 12, 10, look());
        let rule = text(&lines[COMMAND_ROWS + 1]);
        assert!(rule.contains(" +33 above · +10 below "), "{rule}");
        assert_eq!(text(&lines[11]), "  line 40");
        for line in &lines {
            assert!(line.width() <= 40, "{}", text(line));
        }
    }

    // The state row reads as the shell's row in the tasks list: its mark
    // and id, then its word and time at the right, which give way whole on
    // a narrow tray; the rule says how much earlier output is not kept.
    #[test]
    fn a_shell_shows_its_newest_output_and_scrolls_back() {
        let mut shell_job = job("bash-1", JobState::Running);
        shell_job.output = (1..=20).map(|n| format!("line {n}\n")).collect();
        shell_job.omitted_bytes = 2048;
        let (lines, most) = shell(&shell_job, Duration::ZERO, Duration::ZERO, 40, 8, 0, look());
        assert_eq!(text(&lines[0]), "  $ run bash-1");
        assert_eq!(
            text(&lines[1]),
            format!("  ● bash-1{}running  5s", " ".repeat(17))
        );
        assert!(
            text(&lines[2]).ends_with(" earlier 2.0 KB · +15 above -"),
            "{}",
            text(&lines[2])
        );
        assert_eq!(lines.len(), 8);
        assert_eq!(text(&lines[7]), "  line 20");
        assert_eq!(most, 15);
        let row = |width| {
            let (lines, _) = shell(
                &shell_job,
                Duration::ZERO,
                Duration::ZERO,
                width,
                8,
                0,
                look(),
            );
            text(&lines[1])
        };
        assert_eq!(row(25), "  ● bash-1  running  5s");
        assert_eq!(row(22), "  ● bash-1   running");
        assert_eq!(row(15), "  ● bash-1");
        // With no room at all the command wraps a cell to a row, and the
        // state row still comes after it whole.
        let (lines, _) = shell(&shell_job, Duration::ZERO, Duration::ZERO, 0, 8, 0, look());
        assert!(lines.iter().any(|line| text(line) == "  ● bash-1"));
        let (lines, _) = shell(
            &shell_job,
            Duration::ZERO,
            Duration::ZERO,
            40,
            8,
            99,
            look(),
        );
        assert_eq!(text(&lines[3]), "  line 1");
        let empty = job("bash-2", JobState::Running);
        let (lines, most) = shell(&empty, Duration::ZERO, Duration::ZERO, 40, 8, 0, look());
        assert_eq!(text(&lines[3]), format!("  {}", copy::JOB_NO_OUTPUT));
        assert_eq!(most, 0);
    }

    #[test]
    fn output_never_moves_the_cursor() {
        let mut noisy = job("bash-1", JobState::Running);
        noisy.output = "\u{1b}[2J\u{1b}[31mred\u{1b}[0m\r\nbell\u{7}\n".into();
        let (lines, _) = shell(&noisy, Duration::ZERO, Duration::ZERO, 40, 10, 0, look());
        for line in &lines {
            let shown = text(line);
            assert!(!shown.chars().any(|c| c.is_control()), "{shown:?}");
        }
    }
}
