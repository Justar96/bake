//! The crew panel: the headless agents the session spawned, drawn directly
//! above the composer while any is reported.
//!
//! One header line names the crew and counts it by state; each agent then
//! takes one line: a state mark, its callsign, what it is doing, the tool
//! call it is in or the last line it wrote, and how long it has been in its
//! state. Marks are ASCII, one column each, so no terminal measures them
//! differently:
//!
//! | State | Mark |
//! |---|---|
//! | starting | `.` `o` `O` `o`, cycling |
//! | working | `|` `/` `-` `\`, spinning, and its callsign shimmers |
//! | asking | `?` |
//! | idle | `+` |
//! | failed | `x` |
//! | exited | `-` |
//!
//! Without motion ([`Tones::moves`], off under `NO_COLOR`) the marks hold
//! their first frame, nothing shimmers, and nothing is coloured. At most [`MAX_ROWS`] agents are listed, the working
//! and asking ones first; the rest are counted on the header.
//!
//! Everything here is pure: the runtime reports rows with their age when
//! sent ([`crate::runtime::RuntimeUpdate::Crew`]), and the view ages them
//! by its own clock.

use std::time::Duration;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity::{self, Hue, Tones};

/// Agents listed at most; more are counted on the header.
pub const MAX_ROWS: usize = 4;

/// How long each spinner frame shows.
pub const FRAME: Duration = Duration::from_millis(120);

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];
const STARTING: [&str; 4] = [".", "o", "O", "o"];

/// Where an agent is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrewState {
    Starting,
    Working,
    Asking,
    Idle,
    Failed(String),
    Exited(Option<String>),
}

impl CrewState {
    fn moves(&self) -> bool {
        matches!(self, Self::Starting | Self::Working)
    }

    /// Working and asking agents are listed first.
    fn rank(&self) -> u8 {
        match self {
            Self::Asking => 0,
            Self::Working => 1,
            Self::Starting => 2,
            Self::Failed(_) => 3,
            Self::Idle => 4,
            Self::Exited(_) => 5,
        }
    }

    fn word(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Working => "working",
            Self::Asking => "asking",
            Self::Idle => "idle",
            Self::Failed(_) => "failed",
            Self::Exited(_) => "exited",
        }
    }
}

/// One spawned agent as the runtime reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrewMember {
    pub id: u64,
    /// Its callsign.
    pub name: String,
    pub state: CrewState,
    /// The tool call it is in.
    pub tool: Option<String>,
    /// The last line it wrote.
    pub line: Option<String>,
    /// Its task, on one line.
    pub task: String,
    /// How long it had been in its state when reported.
    pub age: Duration,
}

/// The rows the panel takes for `crew`: none when empty, else a header
/// and up to [`MAX_ROWS`] agents.
pub fn rows(crew: &[CrewMember]) -> u16 {
    if crew.is_empty() {
        0
    } else {
        u16::try_from(crew.len().min(MAX_ROWS) + 1).unwrap_or(0)
    }
}

/// The frame of a mark at `now`.
fn frame(frames: &[&'static str; 4], now: Duration, motion: bool) -> &'static str {
    if !motion {
        return frames[0];
    }
    let index = (now.as_millis() / FRAME.as_millis()) as usize % frames.len();
    frames[index]
}

fn mark(state: &CrewState, now: Duration, tones: Tones) -> Span<'static> {
    let motion = tones.moves();
    let (text, colour) = match state {
        CrewState::Starting => (frame(&STARTING, now, motion), Color::Cyan),
        CrewState::Working => (frame(&SPINNER, now, motion), activity::RUNNING_ANSI),
        CrewState::Asking => ("?", Color::Magenta),
        CrewState::Idle => ("+", Color::Green),
        CrewState::Failed(_) => ("x", Color::Red),
        CrewState::Exited(_) => ("-", Color::DarkGray),
    };
    Span::styled(text, fg(colour, tones).add_modifier(Modifier::BOLD))
}

/// `colour` as a foreground, or none under `NO_COLOR`.
fn fg(colour: Color, tones: Tones) -> Style {
    match tones {
        Tones::None => Style::new(),
        _ => Style::new().fg(colour),
    }
}

/// `12s`, `3m`, `2h`.
fn age(age: Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// `text` cut to `width` columns, ending in `...` when cut.
fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let mut out = String::new();
    for c in text.chars() {
        let next = format!("{out}{c}");
        if next.width() + 3 > width {
            break;
        }
        out = next;
    }
    out.push_str("...");
    out
}

/// The agents in the order they are listed.
fn ordered(crew: &[CrewMember]) -> Vec<&CrewMember> {
    let mut sorted: Vec<&CrewMember> = crew.iter().collect();
    sorted.sort_by_key(|member| (member.state.rank(), member.id));
    sorted
}

/// The panel's lines at `width`, `elapsed` after the rows were reported.
pub fn lines(
    crew: &[CrewMember],
    width: usize,
    now: Duration,
    elapsed: Duration,
    tones: Tones,
) -> Vec<Line<'static>> {
    if crew.is_empty() || width < 12 {
        return Vec::new();
    }
    let dim = match tones {
        Tones::None => Style::new().add_modifier(Modifier::DIM),
        _ => Style::new().fg(Color::DarkGray),
    };
    let mut header = vec![
        Span::raw("  "),
        Span::styled("crew", Style::new().add_modifier(Modifier::BOLD)),
    ];
    for state in [
        CrewState::Asking,
        CrewState::Working,
        CrewState::Starting,
        CrewState::Idle,
        CrewState::Failed(String::new()),
        CrewState::Exited(None),
    ] {
        let count = crew
            .iter()
            .filter(|member| {
                std::mem::discriminant(&member.state) == std::mem::discriminant(&state)
            })
            .count();
        if count > 0 {
            header.push(Span::styled(format!("  {count} {}", state.word()), dim));
        }
    }
    let hidden = crew.len().saturating_sub(MAX_ROWS);
    if hidden > 0 {
        header.push(Span::styled(format!("  (+{hidden} more)"), dim));
    }
    let used: usize = header.iter().map(|span| span.content.width()).sum();
    let hint = "/telecom  /spawn";
    if used + hint.len() + 4 <= width {
        header.push(Span::raw(" ".repeat(width - used - hint.len() - 2)));
        header.push(Span::styled(hint, dim));
    }
    let mut out = vec![Line::from(header)];

    let listed = ordered(crew);
    let name_width = listed
        .iter()
        .take(MAX_ROWS)
        .map(|member| member.name.width())
        .max()
        .unwrap_or(0)
        .clamp(5, 16)
        .min(width / 3);
    for member in listed.into_iter().take(MAX_ROWS) {
        let mut spans = vec![
            Span::raw("  "),
            mark(&member.state, now, tones),
            Span::raw(" "),
        ];
        let name = fit(&member.name, name_width);
        let pad = " ".repeat(name_width - name.width() + 1);
        if member.state == CrewState::Working {
            spans.extend(activity::shimmer_spans(
                &name,
                now,
                tones,
                Hue::Running,
                true,
            ));
        } else {
            let style = match member.state {
                CrewState::Exited(_) => dim,
                _ => Style::new().add_modifier(Modifier::BOLD),
            };
            spans.push(Span::styled(name, style));
        }
        spans.push(Span::raw(pad));
        let status = match (&member.state, &member.tool) {
            (CrewState::Working, Some(tool)) => format!("{tool:<8}"),
            (state, _) => format!("{:<8}", state.word()),
        };
        spans.push(Span::styled(status, dim));
        let age_text = age(member.age + elapsed);
        let used: usize = spans.iter().map(|span| span.content.width()).sum();
        let room = width.saturating_sub(used + age_text.len() + 3);
        let detail = match &member.state {
            CrewState::Failed(reason) => reason.clone(),
            CrewState::Exited(Some(reason)) => reason.clone(),
            _ => member.line.clone().unwrap_or_else(|| member.task.clone()),
        };
        let detail = fit(&detail, room);
        let detail_style = match member.state {
            CrewState::Failed(_) => fg(Color::Red, tones),
            CrewState::Asking => fg(Color::Magenta, tones),
            _ => Style::new(),
        };
        let gap = room.saturating_sub(detail.width());
        spans.push(Span::raw(" "));
        spans.push(Span::styled(detail, detail_style));
        spans.push(Span::raw(" ".repeat(gap + 1)));
        spans.push(Span::styled(age_text, dim));
        out.push(Line::from(clip(spans, width)));
    }
    out[0] = Line::from(clip(std::mem::take(&mut out[0].spans), width));
    out
}

/// `spans` cut to `width` columns.
fn clip(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut left = width;
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        if left == 0 {
            break;
        }
        let columns = span.content.width();
        if columns <= left {
            left -= columns;
            out.push(span);
            continue;
        }
        let mut cut = String::new();
        for c in span.content.chars() {
            let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if w > left {
                break;
            }
            left -= w;
            cut.push(c);
        }
        out.push(Span::styled(cut, span.style));
        break;
    }
    out
}

/// How long after `now` the panel next changes: the next spinner frame
/// while an agent moves and motion is on, else the next second of its
/// clocks. `None` when empty.
pub fn next_change(crew: &[CrewMember], now: Duration, tones: Tones) -> Option<Duration> {
    if crew.is_empty() {
        return None;
    }
    let moving = tones.moves() && crew.iter().any(|member| member.state.moves());
    let step = if moving {
        FRAME
    } else {
        activity::next_change(now, false).max(Duration::from_millis(1))
    };
    if moving {
        let into = now.as_millis() % step.as_millis();
        Some(Duration::from_millis(
            u64::try_from(step.as_millis() - into).unwrap_or(1),
        ))
    } else {
        Some(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: u64, name: &str, state: CrewState) -> CrewMember {
        CrewMember {
            id,
            name: name.into(),
            state,
            tool: None,
            line: Some("running the tests".into()),
            task: "write tests".into(),
            age: Duration::from_secs(5),
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn the_panel_lists_busy_agents_first_with_ascii_marks() {
        let mut working = member(2, "alpha-2", CrewState::Working);
        working.tool = Some("bash".into());
        let crew = vec![
            member(1, "alpha-1", CrewState::Idle),
            working,
            member(3, "alpha-3", CrewState::Asking),
        ];
        assert_eq!(rows(&crew), 4);
        let lines = lines(
            &crew,
            60,
            Duration::ZERO,
            Duration::from_secs(60),
            Tones::Ansi,
        );
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert!(
            texts[0].starts_with("  crew  1 asking  1 working  1 idle  "),
            "{:?}",
            texts[0]
        );
        assert!(texts[0].ends_with("/telecom  /spawn"), "{:?}", texts[0]);
        assert!(
            texts[1].starts_with("  ? alpha-3 asking   running the tests"),
            "{:?}",
            texts[1]
        );
        assert!(
            texts[2].starts_with("  | alpha-2 bash     running the tests"),
            "{:?}",
            texts[2]
        );
        assert!(texts[3].starts_with("  + alpha-1 idle"), "{:?}", texts[3]);
        assert!(texts[3].ends_with(" 1m"), "{:?}", texts[3]);
        for line in &texts {
            assert!(line.is_ascii(), "{line:?}");
            assert!(line.width() <= 60, "{line:?}");
        }
    }

    #[test]
    fn marks_spin_only_with_motion() {
        let crew = vec![member(1, "a", CrewState::Working)];
        let at = |now: u64, tones| {
            text(&lines(&crew, 40, Duration::from_millis(now), Duration::ZERO, tones)[1])
        };
        assert!(at(0, Tones::Ansi).starts_with("  | "));
        assert!(at(120, Tones::Ansi).starts_with("  / "));
        assert!(at(360, Tones::Ansi).starts_with("  \\ "));
        assert!(at(120, Tones::None).starts_with("  | "));
        assert_eq!(
            next_change(&crew, Duration::from_millis(50), Tones::Ansi),
            Some(Duration::from_millis(70))
        );
        assert_eq!(next_change(&[], Duration::ZERO, Tones::Ansi), None);
        let idle = vec![member(1, "a", CrewState::Idle)];
        assert_eq!(
            next_change(&idle, Duration::from_millis(400), Tones::Ansi),
            Some(Duration::from_millis(600))
        );
    }

    #[test]
    fn long_crews_and_narrow_screens_stay_in_bounds() {
        let crew: Vec<CrewMember> = (0..7)
            .map(|id| member(id, &format!("a-very-long-callsign-{id}"), CrewState::Idle))
            .collect();
        assert_eq!(rows(&crew), 5);
        for width in [12usize, 20, 33, 80] {
            let lines = lines(&crew, width, Duration::ZERO, Duration::ZERO, Tones::None);
            assert!(text(&lines[0]).contains("(+3 more)") || width < 40);
            for line in &lines {
                assert!(text(line).width() <= width, "{width}: {:?}", text(line));
            }
        }
    }
}
