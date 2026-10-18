//! Background shells: the row under the composer while any runs, the list
//! it opens, and one shell's output.
//!
//! The runtime reports every job its session retains
//! ([`crate::runtime::RuntimeUpdate::Jobs`]), in start order, each with its
//! newest output. Jobs belong to the process: a new session, or a restart,
//! starts with none, so nothing here outlives the runtime that reported
//! it. The view draws what was reported and asks the runtime to stop a job
//! ([`crate::state::Effect::StopJob`]); the runtime reports the stop.

use std::time::Duration;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};

use crate::activity::{self, Tones};
use crate::copy;
use crate::shell_output;
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

/// The job's mark and the colour that says how it is doing: running,
/// stopping, a clean exit, a failing exit or a failure, and a stop.
fn mark(state: &JobState, classic: bool, tones: Tones) -> Span<'static> {
    let (round, ascii, color) = match state {
        JobState::Running => ("●", "*", Color::Cyan),
        JobState::Stopping => ("◌", "~", Color::Yellow),
        JobState::Completed(detail)
            if exit_code(detail.as_deref()).is_none_or(|code| code == 0) =>
        {
            ("✓", "+", Color::Green)
        }
        JobState::Completed(_) | JobState::Failed(_) => ("✗", "x", Color::Red),
        JobState::Killed(_) => ("■", "#", Color::Yellow),
    };
    let glyph = if classic { ascii } else { round };
    match tones {
        Tones::None => Span::raw(glyph),
        _ => Span::styled(glyph, Style::new().fg(color)),
    }
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

fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}

/// `text` cut to `width` cells, with `…` for what was cut.
fn fit(text: &str, width: usize) -> String {
    let one_line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    crate::render::fit(&one_line, width)
}

/// The jobs in the order the list shows them: newest first.
pub fn shown(jobs: &[BackgroundJob]) -> impl Iterator<Item = &BackgroundJob> {
    jobs.iter().rev()
}

/// The selected job: `selected`, else the newest.
pub fn selected<'a>(
    jobs: &'a [BackgroundJob],
    selected: Option<&str>,
) -> Option<&'a BackgroundJob> {
    selected
        .and_then(|id| jobs.iter().find(|job| job.id == id))
        .or_else(|| shown(jobs).next())
}

/// The row under the composer: the one running shell's id, command, and
/// time, or how many run and the newest, with the key that opens the list
/// on the right while the composer has focus.
pub fn standing(
    jobs: &[BackgroundJob],
    at: Duration,
    now: Duration,
    width: usize,
    look: Look,
    with_key: bool,
) -> (Line<'static>, Line<'static>) {
    let live: Vec<&BackgroundJob> = shown(jobs).filter(|job| job.state.is_live()).collect();
    let key = if with_key {
        Line::styled(copy::JOBS_KEY, dim())
    } else {
        Line::default()
    };
    let Some(newest) = live.first() else {
        return (Line::default(), key);
    };
    let mut spans = vec![
        mark(&newest.state, look.classic, look.tones),
        Span::raw(" "),
    ];
    if live.len() > 1 {
        spans.push(Span::styled(
            format!("{} {}", live.len(), copy::JOBS_RUNNING),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" · ", dim()));
    }
    let time = format!("  {}", activity::format_elapsed(elapsed(newest, at, now)));
    spans.push(Span::styled(newest.id.clone(), accent()));
    let used: usize = Line::from(spans.clone()).width() + time.len() + 2 + key.width() + 2;
    spans.push(Span::raw("  "));
    spans.push(Span::raw(fit(
        &newest.command,
        width.saturating_sub(used).max(1),
    )));
    spans.push(Span::styled(time, dim()));
    (Line::from(spans), key)
}

/// The list of background shells, newest first, the selected one marked.
pub fn list(
    jobs: &[BackgroundJob],
    selected_id: Option<&str>,
    at: Duration,
    now: Duration,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let chosen = selected(jobs, selected_id).map(|job| job.id.clone());
    let marker = if look.classic { ">" } else { "›" };
    let id_cells = shown(jobs).map(|job| job.id.len()).max().unwrap_or(0);
    let mut lines = vec![Line::styled(format!("  {}", copy::JOBS_SUBTITLE), dim())];
    for job in shown(jobs) {
        let picked = chosen.as_deref() == Some(job.id.as_str());
        let status = format!(
            "{:>9}  {:>7}",
            word(&job.state),
            activity::format_elapsed(elapsed(job, at, now))
        );
        let lead = 2 + 2 + id_cells + 2;
        let room = width.saturating_sub(lead + status.len() + 2).max(1);
        let command = fit(&job.command, room);
        let pad = room.saturating_sub(unicode_width::UnicodeWidthStr::width(command.as_str()));
        let text_style = if picked {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", if picked { marker } else { " " }),
                if picked { accent() } else { Style::new() },
            ),
            mark(&job.state, look.classic, look.tones),
            Span::raw(" "),
            Span::styled(
                format!("{:<id_cells$}", job.id),
                if picked { accent() } else { dim() },
            ),
            Span::raw("  "),
            Span::styled(command, text_style),
            Span::raw(" ".repeat(pad + 2)),
            Span::styled(status, dim()),
        ]));
    }
    lines
}

/// One shell: its id and command, how it is doing, a rule, then its newest
/// output in at most `height` rows, `scroll` rows up from its end. Returns
/// the rows and the most `scroll` can be.
pub fn shell(
    job: &BackgroundJob,
    at: Duration,
    now: Duration,
    width: usize,
    height: usize,
    scroll: usize,
    look: Look,
) -> (Vec<Line<'static>>, usize) {
    let mut status = vec![
        Span::raw("  "),
        mark(&job.state, look.classic, look.tones),
        Span::raw(" "),
        Span::raw(word(&job.state)),
        Span::styled(
            format!(" · {}", activity::format_elapsed(elapsed(job, at, now))),
            dim(),
        ),
    ];
    if job.omitted_bytes > 0 {
        status.push(Span::styled(
            format!(" · {} {}", copy::JOB_EARLIER, bytes(job.omitted_bytes)),
            dim(),
        ));
    }
    let mut lines: Vec<Line<'static>> = transcript::wrap(&job.command, width.saturating_sub(4))
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            Line::from(vec![
                Span::raw(if i == 0 { "  $ " } else { "    " }),
                Span::styled(part, Style::new().add_modifier(Modifier::BOLD)),
            ])
        })
        .collect();
    lines.push(Line::from(status));
    lines.push(transcript::rule(width, transcript::RAIL, look));
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
    let room = height.saturating_sub(lines.len());
    let most = output.len().saturating_sub(room);
    let scroll = scroll.min(most);
    let end = output.len() - scroll;
    lines.extend(output.drain(end.saturating_sub(room)..end));
    (lines, most)
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

    #[test]
    fn the_standing_row_shows_one_shell_or_counts_several() {
        let at = Duration::ZERO;
        let mut jobs = vec![job("bash-1", JobState::Running)];
        let (left, right) = standing(&jobs, at, at, 80, look(), true);
        assert_eq!(text(&left), "● bash-1  run bash-1  5s");
        assert_eq!(text(&right), copy::JOBS_KEY);
        jobs.push(job("bash-2", JobState::Running));
        jobs.push(job("bash-3", JobState::Completed(None)));
        let (left, _) = standing(&jobs, at, at, 80, look(), false);
        assert_eq!(text(&left), "● 2 shells running · bash-2  run bash-2  5s");
        let finished = vec![job("bash-1", JobState::Killed(None))];
        assert_eq!(text(&standing(&finished, at, at, 80, look(), true).0), "");
    }

    #[test]
    fn the_list_is_newest_first_with_the_selection_marked() {
        let jobs = vec![
            job("bash-1", JobState::Completed(Some("exit code: 0".into()))),
            job("bash-2", JobState::Running),
        ];
        let lines = list(&jobs, None, Duration::ZERO, Duration::ZERO, 60, look());
        assert_eq!(lines.len(), 3);
        assert!(
            text(&lines[1]).starts_with("› ● bash-2  run bash-2"),
            "{}",
            text(&lines[1])
        );
        assert!(
            text(&lines[1]).ends_with("running       5s"),
            "{}",
            text(&lines[1])
        );
        assert!(
            text(&lines[2]).starts_with("  ✓ bash-1"),
            "{}",
            text(&lines[2])
        );
        assert!(text(&lines[2]).contains("exit 0"));
        let lines = list(
            &jobs,
            Some("bash-1"),
            Duration::ZERO,
            Duration::ZERO,
            60,
            look(),
        );
        assert!(text(&lines[2]).starts_with("› ✓ bash-1"));
        for line in &lines {
            assert!(line.width() <= 60, "{}", text(line));
        }
    }

    #[test]
    fn a_shell_shows_its_newest_output_and_scrolls_back() {
        let mut shell_job = job("bash-1", JobState::Running);
        shell_job.output = (1..=20).map(|n| format!("line {n}\n")).collect();
        shell_job.omitted_bytes = 2048;
        let (lines, most) = shell(&shell_job, Duration::ZERO, Duration::ZERO, 40, 8, 0, look());
        assert_eq!(text(&lines[0]), "  $ run bash-1");
        assert_eq!(text(&lines[1]), "  ● running · 5s · earlier 2.0 KB");
        assert_eq!(lines.len(), 8);
        assert_eq!(text(&lines[7]), "  line 20");
        assert_eq!(most, 15);
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
