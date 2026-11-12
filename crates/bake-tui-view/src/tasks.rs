//! Tasks: the work that runs beside the conversation, the session's
//! subagents ([`crate::agents`]) and background shells ([`crate::jobs`]), as
//! one surface under the composer.
//!
//! Bake's own design. While the tray is closed, one standing row says what
//! is in flight ([`standing`]). Ctrl+G, or ↓ past the draft's last row,
//! opens the tray on one list of both kinds in two groups, agents then
//! shells, with one selection that moves across them ([`list`]); Enter
//! opens the selected task's own view, an agent's [`crate::agents::ledger`]
//! or a shell's [`crate::jobs::shell`]. A task's mark is the transcript's
//! mark for a call in the same state ([`crate::transcript::mark_of`]), in
//! the same glyph and colour, and done, failed, and stopped read alike in
//! both places. A running task's dot holds still: only the transcript's
//! running call pulses, so the tasks never show a blank cell for a state.

use std::ops::Range;
use std::time::Duration;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity::{self, Tones};
use crate::agents::{self, Agent, AgentState, Ended};
use crate::copy;
use crate::jobs::{self, BackgroundJob, JobState};
use crate::render::{fit, pad_start};
use crate::shell_output;
use crate::transcript::{self, CallState, Look};

/// A task by its kind and id: a subagent's session id, or a background
/// shell's job id, such as `bash-1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskId {
    Agent(String),
    Shell(String),
}

impl TaskId {
    pub fn group(&self) -> Group {
        match self {
            Self::Agent(_) => Group::Agents,
            Self::Shell(_) => Group::Shells,
        }
    }
}

/// The list's groups, in the order it shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Agents,
    Shells,
}

/// How a task is doing, as its mark and the standing row's counts say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// At work: an agent starting or working, a shell running or stopping.
    Running,
    Done,
    /// A run that failed, or a shell that exited with a code other than 0.
    Failed,
    Stopped,
    /// An agent at rest whose run in this process has not ended yet.
    Resting,
    /// An agent whose session file cannot be read.
    Unreadable,
}

fn agent_phase(state: AgentState) -> Phase {
    match (state, state.ended()) {
        (AgentState::Starting | AgentState::Working, _) => Phase::Running,
        (AgentState::Unavailable, _) => Phase::Unreadable,
        (_, Some(Ended::Completed)) => Phase::Done,
        (_, Some(Ended::Failed)) => Phase::Failed,
        (_, Some(Ended::Stopped)) => Phase::Stopped,
        _ => Phase::Resting,
    }
}

/// A shell's phase: a requested stop still runs, and an exit other than 0
/// failed.
pub fn shell_phase(state: &JobState) -> Phase {
    match state {
        JobState::Running | JobState::Stopping => Phase::Running,
        JobState::Killed(_) => Phase::Stopped,
        _ if state.failed() => Phase::Failed,
        _ => Phase::Done,
    }
}

/// An agent's phase, from its state.
pub fn phase_of(state: AgentState) -> Phase {
    agent_phase(state)
}

/// The mark for `phase`: a call's running dot (lit, as the caller's
/// [`Look`] holds it), its done check and failed cross, and a cancelled call's
/// mark for a stop, each in its call's colour. An agent at rest takes a
/// dim hollow dot and an unreadable one a red `?`, so every phase keeps its
/// own shape under `NO_COLOR` and in the classic frame's ASCII.
pub fn mark(phase: Phase, look: Look) -> Span<'static> {
    let (glyph, style) = match phase {
        Phase::Running => transcript::mark_of(CallState::Running, look),
        Phase::Done => transcript::mark_of(CallState::Done, look),
        Phase::Failed => transcript::mark_of(CallState::Failed, look),
        Phase::Stopped => (look.marks().cancelled, transcript::yellow(look)),
        Phase::Resting => (if look.classic { "o" } else { "○" }, dim()),
        Phase::Unreadable => ("?", transcript::red(look)),
    };
    Span::styled(glyph, style)
}

/// One task as the standing row and the list draw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task<'a> {
    Agent(&'a Agent),
    Shell(&'a BackgroundJob),
}

impl<'a> Task<'a> {
    pub fn id(self) -> TaskId {
        match self {
            Self::Agent(agent) => TaskId::Agent(agent.id.clone()),
            Self::Shell(job) => TaskId::Shell(job.id.clone()),
        }
    }

    /// Whether `id` names this task.
    pub fn is(self, id: &TaskId) -> bool {
        match (self, id) {
            (Self::Agent(agent), TaskId::Agent(id)) => &agent.id == id,
            (Self::Shell(job), TaskId::Shell(id)) => &job.id == id,
            _ => false,
        }
    }

    pub fn group(self) -> Group {
        match self {
            Self::Agent(_) => Group::Agents,
            Self::Shell(_) => Group::Shells,
        }
    }

    pub fn phase(self) -> Phase {
        match self {
            Self::Agent(agent) => agent_phase(agent.state),
            Self::Shell(job) => shell_phase(&job.state),
        }
    }

    /// What a row names it by: an agent's label, a shell's id. An agent's
    /// session id is too long to scan; its ledger shows it.
    fn name(self) -> &'a str {
        match self {
            Self::Agent(agent) => &agent.name,
            Self::Shell(job) => &job.id,
        }
    }

    /// What it is: a shell's command; an agent's model, after where the
    /// agent rests once a run has ended, `idle · faux/faux-2`.
    fn what(self) -> String {
        match self {
            Self::Shell(job) => job.command.clone(),
            Self::Agent(agent) => {
                let rest = match agent.state {
                    AgentState::Idle(Some(_)) => Some(copy::AGENT_IDLE),
                    AgentState::Saved(Some(_)) => Some(copy::AGENT_SAVED),
                    _ => None,
                };
                [rest, agent.route.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(copy::STATUS_SEP)
            }
        }
    }

    /// How it is doing in one word, the same words for both kinds where
    /// they mean the same: `running`, `exit 0`, `working`, `done`,
    /// `failed`, `stopped`, `idle`.
    fn word(self) -> String {
        match self {
            Self::Shell(job) => jobs::word(&job.state),
            Self::Agent(agent) => match (agent.state, agent.state.ended()) {
                (AgentState::Starting, _) => copy::AGENT_STARTING.to_owned(),
                (AgentState::Working, _) => copy::AGENT_WORKING.to_owned(),
                (AgentState::Unavailable, _) => copy::AGENT_UNREADABLE.to_owned(),
                (_, Some(ended)) => agents::ended_word(ended).to_owned(),
                (AgentState::Saved(None), _) => copy::AGENT_SAVED.to_owned(),
                _ => copy::AGENT_IDLE.to_owned(),
            },
        }
    }

    /// How long it has run, where the runtime reports it: a shell's time.
    /// The runtime reports no time for an agent.
    fn time(self, at: Duration, now: Duration) -> Option<String> {
        match self {
            Self::Shell(job) => Some(activity::format_elapsed(jobs::elapsed(job, at, now))),
            Self::Agent(_) => None,
        }
    }

    /// The one dim row the list shows under the task while it is
    /// selected: an agent's last words once its run has ended, a shell's
    /// newest line of output. Both are untrusted, so escape sequences and
    /// control characters are taken out.
    fn detail(self) -> Option<String> {
        let text = match self {
            Self::Agent(agent) if agent_phase(agent.state) != Phase::Running => {
                one_line(agent.detail.as_deref()?)
            }
            Self::Agent(_) => return None,
            Self::Shell(job) => job
                .output
                .split('\n')
                .rev()
                .map(|line| {
                    let text = shell_output::styled(line, shell_output::Family::Other, Tones::None)
                        .text
                        .replace('\t', "    ");
                    text.trim().to_owned()
                })
                .find(|line| !line.is_empty())?,
        };
        (!text.is_empty()).then_some(text)
    }
}

/// `text` from the runtime on one row: escape sequences and other control
/// characters, which could move the cursor or restyle the terminal, taken
/// out, and each run of white space one space.
pub(crate) fn one_line(text: &str) -> String {
    let plain = shell_output::parse_sgr(&text.replace(['\n', '\r', '\t'], " "), Tones::None).0;
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The tasks in the order the list shows them: the agents, then the
/// shells, and in each group the running ones before the rest, newest
/// first. The runtime reports both kinds in start order.
pub fn order<'a>(agents: &'a [Agent], jobs: &'a [BackgroundJob]) -> Vec<Task<'a>> {
    let groups = [
        agents.iter().rev().map(Task::Agent).collect::<Vec<_>>(),
        jobs.iter().rev().map(Task::Shell).collect(),
    ];
    let mut tasks = Vec::with_capacity(agents.len() + jobs.len());
    for group in groups {
        let (running, rest): (Vec<_>, Vec<_>) = group
            .into_iter()
            .partition(|task| task.phase() == Phase::Running);
        tasks.extend(running);
        tasks.extend(rest);
    }
    tasks
}

/// The task `id` names, else the first listed.
pub fn selected<'a>(tasks: &[Task<'a>], id: Option<&TaskId>) -> Option<Task<'a>> {
    id.and_then(|id| tasks.iter().copied().find(|task| task.is(id)))
        .or_else(|| tasks.first().copied())
}

/// The first task of `group` as listed, else the first task.
pub fn first_of<'a>(tasks: &[Task<'a>], group: Group) -> Option<Task<'a>> {
    tasks
        .iter()
        .copied()
        .find(|task| task.group() == group)
        .or_else(|| tasks.first().copied())
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// The tray's one accent: cyan, or bold without colour.
fn accent(tones: Tones) -> Style {
    match tones {
        Tones::None => Style::new().add_modifier(Modifier::BOLD),
        _ => Style::new().fg(Color::Cyan),
    }
}

/// The standing row's and the bar's name for the tasks: the preview says
/// its tasks are samples.
pub fn label(live: bool) -> &'static str {
    if live {
        copy::TASKS
    } else {
        copy::SAMPLE_TASKS
    }
}

/// One count of the standing row: `2 running`, `1 failed`.
struct Count {
    text: String,
    style: Style,
    /// Whether it counts failures, which stay longer than the dim counts.
    failed: bool,
}

/// The standing row's counts, in the order it shows them: what runs, in
/// the terminal's foreground; what failed, in red, as the one that asks
/// for a look; then, dim, what is done, stopped, idle, saved, or
/// unreadable. Counts of none are left out.
fn counts(tasks: &[Task], look: Look) -> Vec<Count> {
    let words = [
        (copy::TASK_RUNNING, Style::new()),
        (copy::AGENT_FAILED, transcript::red(look)),
        (copy::AGENT_DONE, dim()),
        (copy::AGENT_STOPPED, dim()),
        (copy::AGENT_IDLE, dim()),
        (copy::AGENT_SAVED, dim()),
        (copy::AGENT_UNREADABLE, dim()),
    ];
    let mut tally = [0usize; 7];
    for task in tasks {
        let slot = match task.phase() {
            Phase::Running => 0,
            Phase::Failed => 1,
            Phase::Done => 2,
            Phase::Stopped => 3,
            Phase::Resting => match task {
                Task::Agent(agent) if matches!(agent.state, AgentState::Saved(_)) => 5,
                _ => 4,
            },
            Phase::Unreadable => 6,
        };
        tally[slot] += 1;
    }
    words
        .into_iter()
        .zip(tally)
        .filter(|(_, n)| *n > 0)
        .map(|((word, style), n)| Count {
            text: format!("{n} {word}"),
            style,
            failed: word == copy::AGENT_FAILED,
        })
        .collect()
}

/// Fewest cells of the newest task's command, or of an agent's name, the
/// standing row keeps; with less, they give way whole.
const FLEX_MIN: usize = 8;

/// The parts of the standing row a width keeps.
#[derive(Clone, Copy, Debug)]
struct Kept {
    label: bool,
    /// Counts after the first, other than the failed one, kept from the
    /// front.
    extras: usize,
    newest: bool,
    /// The newest shell's command, or the newest agent's name.
    what: bool,
    time: bool,
    failed: bool,
}

/// The row under the composer: the label, how many tasks run and how many
/// failed or finished, then the newest running task, its mark, name or id,
/// command, and time; with `tail`, such as the key that opens the tray,
/// dim at the right. Nothing but the tail while there are no tasks.
///
/// Narrower, it gives up its least important parts first, each whole: the
/// command shrinks to [`FLEX_MIN`] cells, then the dim counts go from the
/// last, then the command, the time, the label, the newest task, and the
/// failed count. The first count stays, and is never cut: on a row too
/// narrow for it beside the tail, the tail gives way.
#[allow(clippy::too_many_arguments)]
pub fn standing(
    tasks: &[Task],
    newest: Option<Task>,
    at: Duration,
    now: Duration,
    width: usize,
    look: Look,
    live: bool,
    tail: Option<&str>,
) -> (Line<'static>, Line<'static>) {
    let tail = tail.map_or_else(Line::default, |text| Line::styled(one_line(text), dim()));
    if tasks.is_empty() {
        return (Line::default(), tail);
    }
    let counts = counts(tasks, look);
    // The first count always shows; the failed one, when it is not first,
    // stays longer than the dim ones.
    let failed_at = counts
        .iter()
        .position(|count| count.failed)
        .filter(|at| *at > 0);
    let extras = counts.len() - 1 - usize::from(failed_at.is_some());
    let mut kept = Kept {
        label: true,
        extras,
        newest: newest.is_some(),
        what: true,
        time: true,
        failed: true,
    };
    let mut levels = vec![kept];
    while kept.extras > 0 {
        kept.extras -= 1;
        levels.push(kept);
    }
    for step in 0..5 {
        match step {
            0 => kept.what = false,
            1 => kept.time = false,
            2 => kept.label = false,
            3 => kept.newest = false,
            _ => kept.failed = false,
        }
        levels.push(kept);
    }
    let beside = match tail.width() {
        0 => width,
        cells => width.saturating_sub(cells + 2),
    };
    for (room, tail) in [(beside, tail), (width, Line::default())] {
        for kept in &levels {
            if let Some(line) =
                standing_row(&counts, failed_at, newest, *kept, at, now, room, look, live)
            {
                return (line, tail);
            }
        }
    }
    (Line::default(), Line::default())
}

/// The standing row with the parts `kept` keeps, or `None` when they do
/// not fit in `room`.
#[allow(clippy::too_many_arguments)]
fn standing_row(
    counts: &[Count],
    failed_at: Option<usize>,
    newest: Option<Task>,
    kept: Kept,
    at: Duration,
    now: Duration,
    room: usize,
    look: Look,
    live: bool,
) -> Option<Line<'static>> {
    let mut head = Vec::new();
    if kept.label {
        // Once nothing runs or failed, the whole row is dim: what finished
        // stays in reach without asking for a look.
        let quiet = counts.iter().all(|count| count.style == dim());
        head.push(Span::styled(
            label(live),
            if quiet {
                dim()
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            },
        ));
    }
    let mut extras = kept.extras;
    let mut shown = 0;
    for (i, count) in counts.iter().enumerate() {
        let keep = if i == 0 {
            true
        } else if Some(i) == failed_at {
            kept.failed
        } else if extras > 0 {
            extras -= 1;
            true
        } else {
            false
        };
        if !keep {
            continue;
        }
        if shown > 0 {
            head.push(Span::styled(copy::STATUS_SEP, dim()));
        } else if !head.is_empty() {
            head.push(Span::raw("  "));
        }
        head.push(Span::styled(count.text.clone(), count.style));
        shown += 1;
    }
    // The newest running task: an agent shows its name, which gives way
    // as its command does a shell's; a shell its id, command, and time.
    let mut flex: Option<(String, Style)> = None;
    let mut tail = Vec::new();
    match newest.filter(|_| kept.newest) {
        Some(task @ Task::Agent(agent)) if kept.what => {
            head.extend([Span::raw("  "), mark(task.phase(), look), Span::raw(" ")]);
            flex = Some((one_line(&agent.name), accent(look.tones)));
        }
        Some(task @ Task::Shell(job)) => {
            head.extend([
                Span::raw("  "),
                mark(task.phase(), look),
                Span::raw(" "),
                Span::styled(one_line(&job.id), accent(look.tones)),
            ]);
            if kept.what {
                head.push(Span::raw("  "));
                flex = Some((one_line(&job.command), Style::new()));
            }
            if kept.time
                && let Some(time) = task.time(at, now)
            {
                tail.push(Span::styled(format!("  {time}"), dim()));
            }
        }
        _ => {}
    }
    let fixed = Line::from(head.clone()).width() + Line::from(tail.clone()).width();
    if fixed > room {
        return None;
    }
    if let Some((text, style)) = flex {
        let cells = room - fixed;
        if cells < FLEX_MIN.min(text.width()) {
            return None;
        }
        head.push(Span::styled(cut(&text, cells), style));
    }
    head.extend(tail);
    Some(Line::from(head))
}

/// `text` cut to `cells`, its ellipsis right after the last cell kept that
/// is not a space.
fn cut(text: &str, cells: usize) -> String {
    let cut = fit(text, cells);
    match cut.strip_suffix('…') {
        Some(kept) if kept.ends_with(' ') => format!("{}…", kept.trim_end()),
        _ => cut,
    }
}

/// `text` cut to `cells` as [`cut`] cuts it, then padded to them.
fn padded(text: &str, cells: usize) -> String {
    let cut = cut(text, cells);
    let fill = cells.saturating_sub(cut.width());
    format!("{cut}{}", " ".repeat(fill))
}

/// Cells between a row's end and the terminal's, where the standing row
/// and the box's contents end too.
const MARGIN: usize = 2;
/// Cells before a row's name: the selection pointer and the mark, each
/// with the cell after it.
const LEAD: usize = 4;
const GAP: usize = 2;
/// The name column's fewest cells before the state word and the time give
/// way, its fewest before the command or model gives way, and its most.
const NAME_MIN: usize = 6;
const NAME_SHARE: usize = 12;
const NAME_MAX: usize = 24;
/// The fewest cells of a command or model worth showing.
const WHAT_MIN: usize = 6;

/// The list's columns: the name's cells, then the command or model's, the
/// state word's, and the time's, each `None` once it has given way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Columns {
    name: usize,
    what: Option<usize>,
    word: Option<usize>,
    time: Option<usize>,
}

/// The columns every row shares at `end` cells. The name takes what its
/// longest needs, up to [`NAME_MAX`], and the command or model the rest;
/// short of room, the name gives up cells down to half the room, but not
/// below [`NAME_SHARE`], then the command gives way and the name takes its
/// room, then the time and the state word go, so a name keeps
/// [`NAME_MIN`] cells where any fit. Once the state word has gone, the
/// name takes the row: the mark still says how the task is doing.
fn columns(tasks: &[Task], at: Duration, now: Duration, end: usize) -> Columns {
    let widest = |cells: &mut dyn Iterator<Item = usize>| cells.max().unwrap_or(0);
    let name_full = widest(&mut tasks.iter().map(|task| one_line(task.name()).width()));
    let word_full = widest(&mut tasks.iter().map(|task| task.word().width()));
    let time_full = tasks
        .iter()
        .filter_map(|task| task.time(at, now))
        .map(|time| time.width())
        .max();
    let name_least = NAME_MIN.min(name_full);
    let right = |word: Option<usize>, time: Option<usize>| {
        word.map_or(0, |w| w + GAP) + time.map_or(0, |w| w + GAP)
    };
    let (word, time) = [(true, true), (true, false)]
        .into_iter()
        .map(|(with_word, with_time)| {
            (
                with_word.then_some(word_full),
                time_full.filter(|_| with_time),
            )
        })
        .find(|(word, time)| end.saturating_sub(LEAD + right(*word, *time)) >= name_least)
        .unwrap_or((None, None));
    let avail = end.saturating_sub(LEAD + right(word, time));
    let mut name = name_full.min(NAME_MAX);
    if avail < name + GAP + WHAT_MIN {
        name = name.min((avail.saturating_sub(GAP) / 2).max(NAME_SHARE));
    }
    let what = word
        .and(avail.checked_sub(name + GAP))
        .filter(|cells| *cells >= WHAT_MIN);
    if what.is_none() {
        name = avail;
    }
    Columns {
        name,
        what,
        word,
        time,
    }
}

/// The tray's list, its rows, and what the tray keeps in view.
#[derive(Debug, Default)]
pub struct Listed {
    pub lines: Vec<Line<'static>>,
    /// The rows that hold the selected task: its row, its detail row, and
    /// its group's label when it is first in its group.
    pub selected: Option<Range<usize>>,
    /// Blank rows the tray keeps under the list, so its height holds while
    /// the selection moves onto a task without a detail row.
    pub reserve: usize,
    /// The line of each task's row, so a tray too short for the list counts
    /// the tasks it leaves out rather than its labels and detail rows.
    pub rows: Vec<usize>,
}

/// The tasks as one list `width` cells wide: each group under a dim
/// label, one row per task, `selected` marked with the pointer and
/// followed by its detail row. Every row has the same columns: the
/// pointer, the mark, the name or id, the command or model, and, right
/// aligned, the state word and the time where one is known.
pub fn list(
    tasks: &[Task],
    selected: Option<&TaskId>,
    at: Duration,
    now: Duration,
    width: usize,
    look: Look,
    live: bool,
) -> Listed {
    let chosen = self::selected(tasks, selected).map(Task::id);
    let end = width.saturating_sub(MARGIN);
    let cols = columns(tasks, at, now, end);
    let pointer = if look.classic { ">" } else { "▸" };
    let mut listed = Listed::default();
    let mut group = None;
    let mut detail_somewhere = false;
    let mut detail_shown = false;
    for task in tasks.iter().copied() {
        let picked = chosen.as_ref().is_some_and(|id| task.is(id));
        // A group's first task keeps its label in view with it.
        let start = listed.lines.len();
        if group != Some(task.group()) {
            group = Some(task.group());
            listed.lines.push(group_label(task.group(), end, live));
        }
        listed.rows.push(listed.lines.len());
        listed
            .lines
            .push(row(task, picked, pointer, cols, at, now, look));
        let detail = task.detail();
        detail_somewhere |= detail.is_some();
        if picked {
            if let Some(detail) = detail {
                detail_shown = true;
                listed.lines.push(Line::from(vec![
                    Span::raw(" ".repeat(LEAD)),
                    Span::styled(fit(&detail, end.saturating_sub(LEAD)), dim()),
                ]));
            }
            listed.selected = Some(start..listed.lines.len());
        }
    }
    listed.reserve = usize::from(detail_somewhere && !detail_shown);
    listed
}

/// A group's dim label, with what to know about its tasks when it fits:
/// that shells stop when Bake exits, or that the preview's are samples.
fn group_label(group: Group, end: usize, live: bool) -> Line<'static> {
    let (name, note) = match (group, live) {
        (Group::Agents, true) => (copy::TASKS_AGENTS, None),
        (Group::Shells, true) => (copy::TASKS_SHELLS, Some(copy::TASKS_SHELLS_NOTE)),
        (Group::Agents, false) => (copy::TASKS_AGENTS, Some(copy::TASKS_SAMPLES_NOTE)),
        (Group::Shells, false) => (copy::TASKS_SHELLS, Some(copy::TASKS_SAMPLES_NOTE)),
    };
    let mut text = name.to_owned();
    if let Some(note) = note
        && GAP + name.width() + copy::STATUS_SEP.width() + note.width() <= end
    {
        text = format!("{name}{}{note}", copy::STATUS_SEP);
    }
    Line::styled(format!("  {}", fit(&text, end.saturating_sub(GAP))), dim())
}

/// One task's row in `cols`.
fn row(
    task: Task,
    picked: bool,
    pointer: &str,
    cols: Columns,
    at: Duration,
    now: Duration,
    look: Look,
) -> Line<'static> {
    let mut spans = vec![
        if picked {
            Span::styled(format!("{pointer} "), accent(look.tones))
        } else {
            Span::raw("  ")
        },
        mark(task.phase(), look),
        Span::raw(" "),
        Span::styled(
            padded(&one_line(task.name()), cols.name),
            if picked {
                accent(look.tones)
            } else {
                Style::new()
            },
        ),
    ];
    if let Some(cells) = cols.what {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(Span::styled(
            padded(&one_line(&task.what()), cells),
            if picked { Style::new() } else { dim() },
        ));
    }
    if let Some(cells) = cols.word {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(Span::styled(pad_start(&task.word(), cells), dim()));
    }
    if let Some(cells) = cols.time {
        spans.push(Span::raw(" ".repeat(GAP)));
        let time = task.time(at, now).unwrap_or_default();
        spans.push(Span::styled(pad_start(&time, cells), dim()));
    }
    Line::from(spans)
}

#[cfg(test)]
pub(crate) mod samples {
    //! Tasks for the view's tests, as a runtime would report them.

    use std::time::Duration;

    use crate::agents::{Agent, AgentState, Ended};
    use crate::jobs::{BackgroundJob, JobState};

    pub fn agent(id: &str, name: &str, state: AgentState) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            state,
            route: Some("faux/faux-2".into()),
            detail: None,
            file: None,
        }
    }

    pub fn shell(id: &str, command: &str, state: JobState, seconds: u64) -> BackgroundJob {
        BackgroundJob {
            id: id.into(),
            command: command.into(),
            state,
            elapsed: Duration::from_secs(seconds),
            output: String::new(),
            omitted_bytes: 0,
        }
    }

    /// One agent of each outcome and one at work, in start order.
    pub fn mixed() -> Vec<Agent> {
        let mut done = agent(
            "reviewer",
            "Review the diff",
            AgentState::Idle(Some(Ended::Completed)),
        );
        done.detail = Some("The change reads well.".into());
        let mut failed = agent(
            "tester",
            "Run the tests",
            AgentState::Saved(Some(Ended::Failed)),
        );
        failed.detail = Some("The request failed.".into());
        vec![
            done,
            failed,
            agent("explorer", "Explore auth", AgentState::Working),
        ]
    }

    /// A finished shell and a running one, in start order.
    pub fn shells() -> Vec<BackgroundJob> {
        let mut built = shell(
            "bash-1",
            "cargo build",
            JobState::Completed(Some("exit code: 0".into())),
            41,
        );
        built.output = "Compiling bake\nFinished release\n".into();
        let mut dev = shell("bash-2", "bun run dev", JobState::Running, 72);
        dev.output = "ready in 312 ms\n".into();
        vec![built, dev]
    }
}

#[cfg(test)]
mod tests {
    use super::samples::{agent, mixed, shell, shells};
    use super::*;

    fn look(tones: Tones, classic: bool, lit: bool) -> Look {
        Look {
            tones,
            classic,
            lit,
        }
    }

    fn plain() -> Look {
        look(Tones::None, false, true)
    }

    fn text(line: &Line<'_>) -> String {
        line.to_string()
    }

    // Every phase keeps its own shape without colour and in ASCII, and in
    // colour a task's mark is the transcript's call mark for that state.
    #[test]
    fn marks_are_the_transcripts_and_tell_phases_apart_without_colour() {
        let phases = [
            Phase::Running,
            Phase::Done,
            Phase::Failed,
            Phase::Stopped,
            Phase::Resting,
            Phase::Unreadable,
        ];
        for classic in [false, true] {
            let shapes: Vec<String> = phases
                .iter()
                .map(|phase| {
                    mark(*phase, look(Tones::None, classic, true))
                        .content
                        .into_owned()
                })
                .collect();
            let expected = if classic {
                ["*", "+", "x", "/", "o", "?"]
            } else {
                ["●", "✓", "✗", "⊘", "○", "?"]
            };
            assert_eq!(shapes, expected, "classic {classic}");
            for shape in &shapes {
                assert_eq!(shape.width(), 1, "{shape}");
            }
        }
        let lit = look(Tones::TrueColor, false, true);
        for (phase, state) in [
            (Phase::Running, CallState::Running),
            (Phase::Done, CallState::Done),
            (Phase::Failed, CallState::Failed),
        ] {
            let call = transcript::mark_of(state, lit);
            let task = mark(phase, lit);
            assert_eq!((task.content.as_ref(), task.style), (call.0, call.1));
        }
    }

    #[test]
    fn phases_and_words_follow_each_kind() {
        let cases = [
            (AgentState::Starting, Phase::Running, "starting"),
            (AgentState::Working, Phase::Running, "working"),
            (AgentState::Idle(None), Phase::Resting, "idle"),
            (AgentState::Saved(None), Phase::Resting, "saved"),
            (
                AgentState::Idle(Some(Ended::Completed)),
                Phase::Done,
                "done",
            ),
            (
                AgentState::Saved(Some(Ended::Failed)),
                Phase::Failed,
                "failed",
            ),
            (
                AgentState::Idle(Some(Ended::Stopped)),
                Phase::Stopped,
                "stopped",
            ),
            (AgentState::Unavailable, Phase::Unreadable, "unreadable"),
        ];
        for (state, phase, word) in cases {
            let agent = agent("a", "A", state);
            let task = Task::Agent(&agent);
            assert_eq!(
                (task.phase(), task.word().as_str()),
                (phase, word),
                "{state:?}"
            );
        }
        let cases = [
            (JobState::Running, Phase::Running, "running"),
            (JobState::Stopping, Phase::Running, "stopping"),
            (
                JobState::Completed(Some("exit code: 0".into())),
                Phase::Done,
                "exit 0",
            ),
            (
                JobState::Completed(Some("exit code: 2".into())),
                Phase::Failed,
                "exit 2",
            ),
            (JobState::Failed(None), Phase::Failed, "failed"),
            (JobState::Killed(None), Phase::Stopped, "stopped"),
        ];
        for (state, phase, word) in cases {
            let job = shell("bash-1", "x", state.clone(), 1);
            let task = Task::Shell(&job);
            assert_eq!(
                (task.phase(), task.word().as_str()),
                (phase, word),
                "{state:?}"
            );
        }
        let done = agent("a", "A", AgentState::Idle(Some(Ended::Completed)));
        assert_eq!(Task::Agent(&done).what(), "idle · faux/faux-2");
        let working = agent("a", "A", AgentState::Working);
        assert_eq!(Task::Agent(&working).what(), "faux/faux-2");
    }

    // Agents come first, then shells; in each group the running ones lead,
    // newest first, and selection falls back to the first task.
    #[test]
    fn the_order_puts_running_tasks_first_in_each_group() {
        let agents = mixed();
        let jobs = shells();
        let tasks = order(&agents, &jobs);
        let ids: Vec<TaskId> = tasks.iter().map(|task| task.id()).collect();
        assert_eq!(
            ids,
            [
                TaskId::Agent("explorer".into()),
                TaskId::Agent("tester".into()),
                TaskId::Agent("reviewer".into()),
                TaskId::Shell("bash-2".into()),
                TaskId::Shell("bash-1".into()),
            ]
        );
        let gone = TaskId::Shell("bash-9".into());
        assert_eq!(
            selected(&tasks, Some(&gone)).map(Task::id),
            Some(ids[0].clone())
        );
        assert_eq!(
            first_of(&tasks, Group::Shells).map(Task::id),
            Some(ids[3].clone())
        );
        let only_shells = order(&[], &jobs);
        assert_eq!(
            first_of(&only_shells, Group::Agents).map(Task::id),
            Some(ids[3].clone())
        );
    }

    fn lines(listed: &Listed) -> Vec<String> {
        listed.lines.iter().map(text).collect()
    }

    // Bake: one list holds both kinds under quiet group labels, every row
    // in the same columns, the selected one followed by its detail.
    #[test]
    fn the_list_aligns_both_kinds_in_one_set_of_columns() {
        let agents = mixed();
        let jobs = shells();
        let tasks = order(&agents, &jobs);
        let at = Duration::ZERO;
        let listed = list(&tasks, None, at, at, 80, plain(), true);
        // The pointer hangs in the margin, so every mark lines up with the
        // labels, and rows end two cells short of the edge, as the box's
        // contents do.
        assert_eq!(
            lines(&listed),
            [
                "  Agents",
                "▸ ● Explore auth     faux/faux-2                               working        ",
                "  ✗ Run the tests    saved · faux/faux-2                        failed        ",
                "  ✓ Review the diff  idle · faux/faux-2                           done        ",
                "  Shells · they stop when Bake exits",
                "  ● bash-2           bun run dev                               running  1m 12s",
                "  ✓ bash-1           cargo build                                exit 0     41s",
            ]
        );
        assert_eq!(listed.selected, Some(0..2));
        // A working agent has no detail row; a shell has one, so the tray
        // keeps a row for it.
        assert_eq!(listed.reserve, 1);
        let one = TaskId::Shell("bash-1".into());
        let listed = list(&tasks, Some(&one), at, at, 80, plain(), true);
        let rows = lines(&listed);
        assert_eq!(
            rows[6],
            "▸ ✓ bash-1           cargo build                                exit 0     41s"
        );
        assert_eq!(rows[7], "    Finished release");
        assert_eq!((listed.selected, listed.reserve), (Some(6..8), 0));
        // A selected group's first task keeps its label in view.
        let listed = list(
            &tasks,
            Some(&TaskId::Shell("bash-2".into())),
            at,
            at,
            80,
            plain(),
            true,
        );
        assert_eq!(listed.selected, Some(4..7));
        // The preview's groups say they are samples.
        let preview = lines(&list(&tasks, None, at, at, 80, plain(), false));
        assert_eq!(preview[0], "  Agents · samples, nothing runs");
        assert_eq!(preview[4], "  Shells · samples, nothing runs");
    }

    // From 40 to 160 columns every row fits, keeps its columns, and gives
    // way whole: the name shrinks toward half the room, then the command or
    // model goes and the name takes its room back, then the time and the
    // state word go.
    #[test]
    fn rows_fit_every_width_and_give_way_whole() {
        let agents = mixed();
        let jobs = shells();
        let tasks = order(&agents, &jobs);
        let at = Duration::ZERO;
        for width in (0..=160).chain([400]) {
            for classic in [false, true] {
                let listed = list(
                    &tasks,
                    None,
                    at,
                    at,
                    width,
                    look(Tones::Ansi, classic, true),
                    true,
                );
                for line in &listed.lines {
                    assert!(
                        line.width() <= width.max(LEAD + 1),
                        "{width}: {:?}",
                        text(line)
                    );
                }
            }
        }
        let at_width = |width| lines(&list(&tasks, None, at, at, width, plain(), true));
        assert_eq!(
            at_width(60)[1..3],
            [
                "▸ ● Explore auth     faux/faux-2           working        ",
                "  ✗ Run the tests    saved · faux/faux-2    failed        ",
            ]
        );
        assert_eq!(
            at_width(60)[5],
            "  ● bash-2           bun run dev           running  1m 12s"
        );
        assert_eq!(at_width(40)[1], "▸ ● Explore auth       working        ");
        assert_eq!(at_width(40)[5], "  ● bash-2             running  1m 12s");
        assert_eq!(at_width(30)[1], "▸ ● Explor…  working        ");
        assert_eq!(at_width(30)[5], "  ● bash-2   running  1m 12s");
        assert_eq!(at_width(20)[1], "▸ ● Explore auth  ");
        assert_eq!(at_width(20)[5], "  ● bash-2        ");
        assert_eq!(
            at_width(160)[1],
            format!(
                "▸ ● Explore auth     faux/faux-2{}working        ",
                " ".repeat(111)
            )
        );
        // A shell's id is short, so its command keeps the room an agent's
        // long name would take.
        let jobs = shells();
        let only = order(&[], &jobs);
        assert_eq!(
            lines(&list(&only, None, at, at, 40, plain(), true))[1],
            "▸ ● bash-2  bun run…   running  1m 12s"
        );
    }

    #[test]
    fn untrusted_names_commands_and_output_reach_no_control_character() {
        let mut job = shell(
            "bash-\u{1b}[2J1",
            "printf 'a\rb'\u{7}\u{1b}]0;x\u{7}",
            JobState::Running,
            1,
        );
        job.output = "ok\n\u{1b}[31mred\u{1b}[0m\tdone\r\n".into();
        let mut child = agent(
            "a\u{9b}",
            "name\u{1b}[H",
            AgentState::Idle(Some(Ended::Failed)),
        );
        child.detail = Some("bad\u{1b}[2J\nthing\u{7}".into());
        let jobs = [job];
        let agents = [child];
        let tasks = order(&agents, &jobs);
        let at = Duration::ZERO;
        for id in [
            TaskId::Agent("a\u{9b}".into()),
            TaskId::Shell("bash-\u{1b}[2J1".into()),
        ] {
            for width in [0, 10, 40, 80] {
                let listed = list(&tasks, Some(&id), at, at, width, plain(), true);
                let (row, _) = standing(
                    &tasks,
                    tasks.first().copied(),
                    at,
                    at,
                    width,
                    plain(),
                    true,
                    Some(copy::TASKS_KEY),
                );
                for line in listed.lines.iter().chain([&row]) {
                    let shown = text(line);
                    assert!(!shown.chars().any(char::is_control), "{shown:?}");
                }
            }
        }
        let listed = list(
            &tasks,
            Some(&TaskId::Shell("bash-\u{1b}[2J1".into())),
            at,
            at,
            80,
            plain(),
            true,
        );
        assert_eq!(lines(&listed).last().unwrap(), "    red    done");
        let listed = list(
            &tasks,
            Some(&TaskId::Agent("a\u{9b}".into())),
            at,
            at,
            80,
            plain(),
            true,
        );
        assert_eq!(lines(&listed)[2], "    bad thing");
    }

    fn row_at(tasks: &[Task], newest: Option<Task>, width: usize, with_key: bool) -> String {
        let at = Duration::ZERO;
        let key = with_key.then_some(copy::TASKS_KEY);
        let (left, key) = standing(tasks, newest, at, at, width, plain(), true, key);
        if key.width() == 0 {
            return text(&left);
        }
        format!("{}|{}", text(&left), text(&key))
    }

    // Bake: the standing row reads what is in flight at a glance, and
    // narrower gives up its least important parts first, each whole; the
    // first count and the key are never cut.
    #[test]
    fn the_standing_row_gives_way_part_by_part() {
        let mut agents = mixed();
        agents.push(agent("old", "Old", AgentState::Idle(Some(Ended::Stopped))));
        let jobs = shells();
        let tasks = order(&agents, &jobs);
        let newest = Some(Task::Shell(&jobs[1]));
        let mut seen = Vec::new();
        for width in [80, 70, 60, 50, 45, 40, 35, 30, 24, 20, 18, 9, 8, 0] {
            seen.push((width, row_at(&tasks, newest, width, false)));
        }
        let expected = [
            (
                80,
                "Tasks  2 running · 1 failed · 2 done · 1 stopped  ● bash-2  bun run dev  1m 12s",
            ),
            (
                70,
                "Tasks  2 running · 1 failed · 2 done  ● bash-2  bun run dev  1m 12s",
            ),
            (
                60,
                "Tasks  2 running · 1 failed  ● bash-2  bun run dev  1m 12s",
            ),
            (50, "Tasks  2 running · 1 failed  ● bash-2  1m 12s"),
            (45, "Tasks  2 running · 1 failed  ● bash-2  1m 12s"),
            (40, "Tasks  2 running · 1 failed  ● bash-2"),
            (35, "2 running · 1 failed  ● bash-2"),
            (30, "2 running · 1 failed  ● bash-2"),
            (24, "2 running · 1 failed"),
            (20, "2 running · 1 failed"),
            (18, "2 running"),
            (9, "2 running"),
            (8, ""),
            (0, ""),
        ]
        .map(|(width, row)| (width, row.to_owned()));
        assert_eq!(seen, expected);
        // The key keeps its cells while the composer has the keyboard.
        assert_eq!(
            row_at(&tasks, newest, 60, true),
            "Tasks  2 running · 1 failed  ● bash-2  1m 12s|Ctrl+G"
        );
        // A working agent as the newest shows its name, cut to fit.
        let newest = Some(Task::Agent(&agents[2]));
        assert_eq!(
            row_at(&tasks, newest, 80, false),
            "Tasks  2 running · 1 failed · 2 done · 1 stopped  ● Explore auth"
        );
        assert_eq!(
            row_at(&tasks, newest, 44, false),
            "Tasks  2 running · 1 failed  ● Explore auth"
        );
        assert_eq!(
            row_at(&tasks, newest, 40, false),
            "Tasks  2 running · 1 failed  ● Explore…"
        );
        assert_eq!(
            row_at(&tasks, newest, 36, false),
            "Tasks  2 running · 1 failed"
        );
        // Nothing running: the failure leads; nothing failed either, the
        // row is a quiet count.
        let resting: Vec<Agent> = agents
            .iter()
            .filter(|a| a.state != AgentState::Working)
            .cloned()
            .collect();
        let tasks = order(&resting, &[]);
        assert_eq!(
            row_at(&tasks, None, 80, true),
            "Tasks  1 failed · 1 done · 1 stopped|Ctrl+G"
        );
        let calm = [agent("a", "A", AgentState::Idle(Some(Ended::Completed)))];
        let tasks = order(&calm, &[]);
        assert_eq!(row_at(&tasks, None, 80, true), "Tasks  1 done|Ctrl+G");
        assert_eq!(row_at(&[], None, 80, true), "|Ctrl+G");
        // The open tray's hidden rows take the tail at any width they fit
        // beside the first count, and give way where they do not.
        let tail = |width| {
            let (left, right) = standing(
                &tasks,
                None,
                Duration::ZERO,
                Duration::ZERO,
                width,
                plain(),
                true,
                Some("+2 below"),
            );
            format!("{}|{}", text(&left), text(&right))
        };
        assert_eq!(tail(40), "Tasks  1 done|+2 below");
        assert_eq!(tail(16), "1 done|+2 below");
        assert_eq!(tail(12), "1 done|");
    }

    // The failed count is red in colour; without colour it is drawn plain
    // while the finished counts are dim, so it still stands out.
    #[test]
    fn the_failed_count_asks_for_a_look() {
        let agents = mixed();
        let tasks = order(&agents, &[]);
        let at = Duration::ZERO;
        for (tones, fg) in [
            (Tones::Ansi, Some(Color::Red)),
            (Tones::TrueColor, Some(Color::Rgb(0xef, 0x44, 0x44))),
            (Tones::None, None),
        ] {
            let (row, _) = standing(
                &tasks,
                None,
                at,
                at,
                80,
                look(tones, false, true),
                true,
                None,
            );
            let failed = row
                .spans
                .iter()
                .find(|s| s.content == "1 failed")
                .expect("failed");
            let done = row
                .spans
                .iter()
                .find(|s| s.content == "1 done")
                .expect("done");
            assert_eq!(failed.style.fg, fg);
            assert!(!failed.style.add_modifier.contains(Modifier::DIM));
            assert!(done.style.add_modifier.contains(Modifier::DIM));
            let label = &row.spans[0];
            assert_eq!(label.content, "Tasks");
            assert!(label.style.add_modifier.contains(Modifier::BOLD));
        }
        // With nothing running or failed, the whole row is dim.
        let calm = [agent("a", "A", AgentState::Idle(Some(Ended::Completed)))];
        let tasks = order(&calm, &[]);
        let (row, _) = standing(&tasks, None, at, at, 80, plain(), true, None);
        assert_eq!(row.to_string(), "Tasks  1 done");
        for span in row.spans.iter().filter(|s| !s.content.trim().is_empty()) {
            assert!(span.style.add_modifier.contains(Modifier::DIM), "{span:?}");
        }
    }
}
