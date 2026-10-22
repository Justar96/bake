//! Agents: the row under the composer, the list Ctrl+G opens, and one
//! agent's read-only ledger.
//!
//! A live session reports the subagents it started
//! ([`crate::runtime::RuntimeUpdate::Agents`]), in start order, each with
//! its state, how its last run in this process ended, and what that run
//! said last. The preview lists fixed [`samples`] that describe no running
//! work. The view draws what was reported and asks the runtime to stop a
//! working agent's run ([`crate::state::Effect::StopAgent`]); the runtime
//! reports the change. The words follow the 0.3 terminal's subagent sheet
//! (`apps/tui/packages/ui/src/subagents.tsx`): how the last run ended, then
//! whether the agent is resident, and the counts the row shows.

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::activity::Tones;
use crate::copy;
use crate::transcript::{self, Look, TagTone};

/// One agent as the runtime reported it, or a preview sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    /// Its id: a sample's name tag, a subagent's session id.
    pub id: String,
    /// Its label.
    pub name: String,
    pub state: AgentState,
    /// `provider/model`; a sample runs on none.
    pub route: Option<String>,
    /// What a sample is for, or what a subagent's last run said: its
    /// closing message, or why it failed.
    pub detail: Option<String>,
    /// Its session file, when it keeps one.
    pub file: Option<String>,
}

/// Where an agent is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// A fixed example; nothing runs.
    Sample,
    /// Being started, or resumed from its file; no run of it can be
    /// stopped yet.
    Starting,
    /// Running.
    Working,
    /// Resident between runs, with how its last run ended, when known.
    Idle(Option<Ended>),
    /// Saved in its file and resumable, not resident.
    Saved(Option<Ended>),
    /// Its file is missing or unreadable.
    Unavailable,
}

/// How an agent's last run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Completed,
    Failed,
    Stopped,
}

impl AgentState {
    /// How the last run ended, while the agent is at rest.
    pub fn ended(self) -> Option<Ended> {
        match self {
            Self::Idle(ended) | Self::Saved(ended) => ended,
            _ => None,
        }
    }
}

/// The fixed examples the preview lists.
pub fn samples() -> Vec<Agent> {
    let sample = |id: &str, name: &str, detail: &str| Agent {
        id: id.into(),
        name: name.into(),
        state: AgentState::Sample,
        route: None,
        detail: Some(detail.into()),
        file: None,
    };
    vec![
        sample(
            "sample-explorer",
            "Sample explorer",
            "Example of a child that would read files for its parent.",
        ),
        sample(
            "sample-reviewer",
            "Sample reviewer",
            "Example of a child that would review a change.",
        ),
    ]
}

/// Whether every agent is a preview sample.
fn all_samples(agents: &[Agent]) -> bool {
    agents.iter().all(|agent| agent.state == AgentState::Sample)
}

/// The selected agent: `selected`, else the first.
pub fn selected<'a>(agents: &'a [Agent], selected: Option<&str>) -> Option<&'a Agent> {
    selected
        .and_then(|id| agents.iter().find(|agent| agent.id == id))
        .or_else(|| agents.first())
}

/// How the agent is doing, outcome first: `starting`, `working`,
/// `done · idle`, `failed · saved`, `saved`, or `unreadable`.
pub fn word(state: AgentState) -> String {
    let rest = match state {
        AgentState::Sample => return copy::LEDGER_STATE_VALUE.to_owned(),
        AgentState::Starting => return copy::AGENT_STARTING.to_owned(),
        AgentState::Working => return copy::AGENT_WORKING.to_owned(),
        AgentState::Unavailable => return copy::AGENT_UNREADABLE.to_owned(),
        AgentState::Idle(_) => copy::AGENT_IDLE,
        AgentState::Saved(_) => copy::AGENT_SAVED,
    };
    match state.ended() {
        Some(ended) => format!("{} · {rest}", ended_word(ended)),
        None => rest.to_owned(),
    }
}

fn ended_word(ended: Ended) -> &'static str {
    match ended {
        Ended::Completed => copy::AGENT_DONE,
        Ended::Failed => copy::AGENT_FAILED,
        Ended::Stopped => copy::AGENT_STOPPED,
    }
}

/// The colour that says how the agent is doing: its outcome once it has
/// one, else whether it works. `None` reads in the terminal's foreground.
fn color(state: AgentState) -> Option<Color> {
    match (state, state.ended()) {
        (AgentState::Starting | AgentState::Working, _) => Some(Color::Cyan),
        (AgentState::Unavailable, _) | (_, Some(Ended::Failed)) => Some(Color::Red),
        (_, Some(Ended::Completed)) => Some(Color::Green),
        (_, Some(Ended::Stopped)) => Some(Color::Yellow),
        _ => None,
    }
}

fn toned(text: impl Into<String>, color: Option<Color>, tones: Tones) -> Span<'static> {
    match (color, tones) {
        (Some(color), Tones::TrueColor | Tones::Ansi) => {
            Span::styled(text.into(), Style::new().fg(color))
        }
        _ => Span::raw(text.into()),
    }
}

/// The agent's mark, a shape for each state so `NO_COLOR` still tells them
/// apart: a starting, working, or resident agent's dot, a saved one's hollow dot, and
/// a finished run's outcome, as the transcript marks a call. A sample has
/// none.
pub fn mark(state: AgentState, classic: bool, tones: Tones) -> Option<Span<'static>> {
    let (round, ascii) = match (state, state.ended()) {
        (AgentState::Sample, _) => return None,
        (AgentState::Unavailable, _) => ("?", "?"),
        (_, Some(Ended::Completed)) => ("✓", "+"),
        (_, Some(Ended::Failed)) => ("✗", "x"),
        (_, Some(Ended::Stopped)) => ("■", "#"),
        (AgentState::Saved(_), None) => ("○", "o"),
        _ => ("●", "*"),
    };
    let glyph = if classic { ascii } else { round };
    let color = match state {
        AgentState::Idle(None) => Some(Color::Blue),
        _ => color(state),
    };
    Some(toned(glyph, color, tones))
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}

/// How many agents work, finished each way, or cannot be read; each agent
/// counts once.
#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    working: usize,
    done: usize,
    failed: usize,
    stopped: usize,
    unreadable: usize,
}

fn counts(agents: &[Agent]) -> Counts {
    let mut counts = Counts::default();
    for agent in agents {
        match (agent.state, agent.state.ended()) {
            (AgentState::Starting | AgentState::Working, _) => counts.working += 1,
            (AgentState::Unavailable, _) => counts.unreadable += 1,
            (_, Some(Ended::Completed)) => counts.done += 1,
            (_, Some(Ended::Failed)) => counts.failed += 1,
            (_, Some(Ended::Stopped)) => counts.stopped += 1,
            _ => {}
        }
    }
    counts
}

/// `Agents  3 · 1 working · 1 done · 1 failed`: the total, then the counts
/// that are not zero, the working count in the running colour and the
/// failed count in red, the one that asks for a look. The preview's
/// samples read `Agents  2 samples · none running`. A count that would pass
/// `width` is left off with the counts after it, so none is ever cut; the
/// total always shows.
pub fn summary(agents: &[Agent], tones: Tones, width: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(
        copy::AGENTS,
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if all_samples(agents) {
        spans.push(Span::styled(
            format!("  {} {}", agents.len(), copy::AGENTS_SUMMARY),
            dim(),
        ));
        return Line::from(spans);
    }
    spans.push(Span::styled(format!("  {}", agents.len()), dim()));
    let counts = counts(agents);
    let parts = [
        (counts.working, copy::AGENT_WORKING, Some(Color::Cyan)),
        (counts.done, copy::AGENT_DONE, None),
        (counts.failed, copy::AGENT_FAILED, Some(Color::Red)),
        (counts.stopped, copy::AGENT_STOPPED, None),
        (counts.unreadable, copy::AGENT_UNREADABLE, None),
    ];
    let mut used: usize = spans.iter().map(Span::width).sum();
    for (count, word, color) in parts.into_iter().filter(|(count, ..)| *count > 0) {
        let text = format!("{count} {word}");
        used += 3 + text.width();
        if used > width {
            break;
        }
        spans.push(Span::styled(" · ", dim()));
        spans.push(match color {
            Some(_) if tones != Tones::None => toned(text, color, tones),
            _ => Span::styled(text, dim()),
        });
    }
    Line::from(spans)
}

/// The row under the composer, `width` cells: the summary, with the key
/// that opens the list on the right while the composer has focus.
pub fn standing(
    agents: &[Agent],
    tones: Tones,
    with_key: bool,
    width: usize,
) -> (Line<'static>, Line<'static>) {
    let key = if with_key {
        Line::styled(copy::AGENTS_KEY, dim())
    } else {
        Line::default()
    };
    if agents.is_empty() {
        return (Line::default(), key);
    }
    let room = if with_key {
        width.saturating_sub(key.width() + 2)
    } else {
        width
    };
    (summary(agents, tones, room), key)
}

/// `text` cut between graphemes so that it and a closing `…` fit in
/// `width` cells.
fn closed(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if used + cells + 1 > width {
            break;
        }
        out.push_str(grapheme);
        used += cells;
    }
    out.push('…');
    out
}

/// `lead`, then an id as a blue tag two cells after it, when it fits.
fn titled(lead: Vec<Span<'static>>, id: &str, width: usize, tones: Tones) -> Line<'static> {
    let mut line = Line::from(lead);
    let tag = transcript::tag(id, TagTone::Blue, tones);
    if line.width() + 2 + tag.width() <= width {
        line.spans.push(Span::raw("  "));
        line.spans.push(tag);
    }
    line
}

/// `text` wrapped at `width` less `indent`, every row indented.
fn indented(text: &str, width: usize, indent: usize, style: Style) -> Vec<Line<'static>> {
    transcript::wrap(text, width.saturating_sub(indent))
        .into_iter()
        .map(|row| {
            Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(row, style),
            ])
        })
        .collect()
}

/// The agents as cards, in start order: a pointer on the selected one, its
/// mark, name, and id tag, and under it what a sample does or how a
/// subagent is doing and on which model.
pub fn list(
    agents: &[Agent],
    selected_id: Option<&str>,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let chosen = selected(agents, selected_id).map(|agent| agent.id.clone());
    let marker = if look.classic { ">" } else { "▸" };
    let subtitle = if all_samples(agents) {
        copy::LIST_SUBTITLE
    } else {
        copy::LIVE_LIST_SUBTITLE
    };
    let mut lines = indented(subtitle, width, 2, dim());
    for agent in agents {
        let picked = chosen.as_deref() == Some(agent.id.as_str());
        let mut lead = vec![Span::raw("  ")];
        if picked {
            lead.push(Span::styled(format!("{marker} "), accent()));
        } else {
            lead.push(Span::raw("  "));
        }
        if let Some(mark) = mark(agent.state, look.classic, look.tones) {
            lead.extend([mark, Span::raw(" ")]);
        }
        lead.push(if picked {
            Span::styled(agent.name.clone(), accent().add_modifier(Modifier::BOLD))
        } else {
            Span::raw(agent.name.clone())
        });
        lines.push(Line::default());
        lines.push(titled(lead, &agent.id, width, look.tones));
        let style = if picked { Style::new() } else { dim() };
        if agent.state == AgentState::Sample {
            let about = agent.detail.as_deref().unwrap_or_default();
            lines.extend(indented(about, width, 4, style));
            continue;
        }
        let about = match &agent.route {
            Some(route) => format!("{} · {route}", word(agent.state)),
            None => word(agent.state),
        };
        lines.extend(indented(&about, width, 4, style));
    }
    lines
}

/// Cells a ledger's label column takes, its gap included.
const LEDGER_LABEL: usize = 8;

/// Rows of a run's last words the ledger shows before it cuts them.
pub const LAST_ROWS: usize = 8;

/// One agent, read only, as a ledger: its name, a dashed rule, then a label
/// and a value per row, each value wrapped under itself. A sample says what
/// it is for; a subagent, its model, state, file, and its last run's words.
pub fn ledger(agent: &Agent, width: usize, look: Look) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(agent.name.clone(), accent().add_modifier(Modifier::BOLD)),
        ]),
        transcript::rule(width, transcript::RAIL, look),
    ];
    let indent = 2 + LEDGER_LABEL;
    let labelled = |label: &str, cells: usize, value: Vec<Span<'static>>| {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{label:<cells$}"), dim()),
        ];
        spans.extend(value);
        Line::from(spans)
    };
    // A tag's padding takes the cell before its column, so its text lines
    // up with the other values.
    lines.push(labelled(
        copy::LEDGER_ID,
        LEDGER_LABEL - 1,
        vec![transcript::tag(&agent.id, TagTone::Blue, look.tones)],
    ));
    let text = |label: &str, value: &str, style: Style, rows: usize| -> Vec<Line<'static>> {
        let mut wrapped = transcript::wrap(value, width.saturating_sub(indent));
        if wrapped.len() > rows {
            wrapped.truncate(rows);
            if let Some(last) = wrapped.last_mut() {
                *last = closed(last, width.saturating_sub(indent));
            }
        }
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, part)| {
                let value = vec![Span::styled(part, style)];
                if i == 0 {
                    labelled(label, LEDGER_LABEL, value)
                } else {
                    let mut spans = vec![Span::raw(" ".repeat(indent))];
                    spans.extend(value);
                    Line::from(spans)
                }
            })
            .collect()
    };
    if agent.state == AgentState::Sample {
        let about = agent.detail.as_deref().unwrap_or_default();
        lines.extend(text(copy::LEDGER_ROLE, about, Style::new(), usize::MAX));
        lines.extend(text(
            copy::LEDGER_STATE,
            copy::LEDGER_STATE_VALUE,
            Style::new(),
            1,
        ));
    } else {
        if let Some(route) = &agent.route {
            lines.extend(text(copy::LEDGER_MODEL, route, Style::new(), usize::MAX));
        }
        let state = match color(agent.state) {
            Some(color) if look.tones != Tones::None => Style::new().fg(color),
            _ => Style::new(),
        };
        lines.extend(text(copy::LEDGER_STATE, &word(agent.state), state, 1));
        if let Some(file) = &agent.file {
            lines.extend(text(copy::LEDGER_FILE, file, dim(), usize::MAX));
        }
        if let Some(detail) = agent.detail.as_deref().filter(|d| !d.trim().is_empty()) {
            let failed = agent.state.ended() == Some(Ended::Failed);
            let style = match look.tones {
                Tones::None => Style::new(),
                _ if failed => Style::new().fg(Color::Red),
                _ => Style::new(),
            };
            // One paragraph: the ledger's rows are the agent's, not its prose.
            let flat = detail.split_whitespace().collect::<Vec<_>>().join(" ");
            lines.extend(text(copy::LEDGER_LAST, &flat, style, LAST_ROWS));
        }
    }
    lines.extend(text(
        copy::LEDGER_INPUT,
        copy::INSPECT_READ_ONLY,
        Style::new().fg(Color::Yellow),
        usize::MAX,
    ));
    lines.extend(text(
        copy::LEDGER_DRAFT,
        copy::INSPECT_PARENT,
        dim(),
        usize::MAX,
    ));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, state: AgentState) -> Agent {
        Agent {
            id: id.into(),
            name: format!("Agent {id}"),
            state,
            route: Some("faux/faux-2".into()),
            detail: None,
            file: None,
        }
    }

    fn look(tones: Tones) -> Look {
        Look {
            tones,
            classic: false,
            lit: true,
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.to_string()
    }

    #[test]
    fn words_lead_with_the_outcome_then_residence() {
        let cases = [
            (AgentState::Starting, "starting"),
            (AgentState::Working, "working"),
            (AgentState::Idle(None), "idle"),
            (AgentState::Idle(Some(Ended::Completed)), "done · idle"),
            (AgentState::Saved(Some(Ended::Failed)), "failed · saved"),
            (AgentState::Saved(Some(Ended::Stopped)), "stopped · saved"),
            (AgentState::Saved(None), "saved"),
            (AgentState::Unavailable, "unreadable"),
            (AgentState::Sample, copy::LEDGER_STATE_VALUE),
        ];
        for (state, expected) in cases {
            assert_eq!(word(state), expected, "{state:?}");
        }
    }

    // Each state keeps its own shape without colour, and a sample has none.
    #[test]
    fn marks_tell_states_apart_without_colour() {
        let shapes: Vec<Option<String>> = [
            AgentState::Working,
            AgentState::Idle(None),
            AgentState::Saved(None),
            AgentState::Idle(Some(Ended::Completed)),
            AgentState::Saved(Some(Ended::Failed)),
            AgentState::Idle(Some(Ended::Stopped)),
            AgentState::Unavailable,
            AgentState::Sample,
        ]
        .into_iter()
        .map(|state| mark(state, false, Tones::None).map(|span| span.content.into_owned()))
        .collect();
        let expected = ["●", "●", "○", "✓", "✗", "■", "?"].map(|glyph| Some(glyph.to_owned()));
        assert_eq!(shapes[..7], expected);
        assert_eq!(shapes[7], None);
        let classic = mark(AgentState::Saved(None), true, Tones::None).expect("a mark");
        assert_eq!(classic.content, "o");
        let lit = mark(AgentState::Working, false, Tones::Ansi).expect("a mark");
        assert_eq!(lit.style.fg, Some(Color::Cyan));
        // In colour, the outcome decides, then whether it works; an idle
        // agent with no run yet is blue and a saved one plain.
        let colours: Vec<Option<Color>> = [
            AgentState::Starting,
            AgentState::Idle(None),
            AgentState::Saved(None),
            AgentState::Idle(Some(Ended::Completed)),
            AgentState::Saved(Some(Ended::Failed)),
            AgentState::Idle(Some(Ended::Stopped)),
            AgentState::Unavailable,
        ]
        .into_iter()
        .map(|state| mark(state, false, Tones::Ansi).and_then(|span| span.style.fg))
        .collect();
        assert_eq!(
            colours,
            [
                Some(Color::Cyan),
                Some(Color::Blue),
                None,
                Some(Color::Green),
                Some(Color::Red),
                Some(Color::Yellow),
                Some(Color::Red),
            ]
        );
    }

    #[test]
    fn the_summary_counts_each_agent_once_and_skips_zero_counts() {
        let agents = [
            agent("a", AgentState::Working),
            agent("b", AgentState::Idle(Some(Ended::Completed))),
            agent("c", AgentState::Saved(Some(Ended::Completed))),
            agent("d", AgentState::Saved(Some(Ended::Failed))),
            agent("e", AgentState::Saved(None)),
            agent("f", AgentState::Unavailable),
        ];
        let line = summary(&agents, Tones::Ansi, usize::MAX);
        assert_eq!(
            text(&line),
            "Agents  6 · 1 working · 2 done · 1 failed · 1 unreadable"
        );
        assert_eq!(
            line.spans[1].style,
            Style::new().add_modifier(Modifier::DIM)
        );
        let mut stopped = agents.to_vec();
        stopped.push(agent("g", AgentState::Idle(Some(Ended::Stopped))));
        stopped.push(agent("h", AgentState::Saved(Some(Ended::Stopped))));
        assert_eq!(
            text(&summary(&stopped, Tones::Ansi, usize::MAX)),
            "Agents  8 · 1 working · 2 done · 1 failed · 2 stopped · 1 unreadable"
        );
        // Without colour every count is dim, the failed one too.
        let plain = summary(&agents, Tones::None, usize::MAX);
        let failed = plain
            .spans
            .iter()
            .find(|span| span.content == "1 failed")
            .expect("failed");
        assert_eq!(failed.style, dim());
        let failed = line
            .spans
            .iter()
            .find(|span| span.content == "1 failed")
            .expect("failed");
        assert_eq!(failed.style.fg, Some(Color::Red));
        assert_eq!(
            text(&summary(&samples(), Tones::Ansi, usize::MAX)),
            format!("Agents  2 {}", copy::AGENTS_SUMMARY)
        );
        let (left, key) = standing(&[], Tones::Ansi, true, 80);
        assert_eq!((text(&left), text(&key)), (String::new(), "Ctrl+G".into()));
        // Counts that would not fit are left off whole, last first; the
        // total stays, and the key keeps its cells.
        for (width, shown) in [
            (
                56,
                "Agents  6 · 1 working · 2 done · 1 failed · 1 unreadable",
            ),
            (55, "Agents  6 · 1 working · 2 done · 1 failed"),
            (41, "Agents  6 · 1 working · 2 done · 1 failed"),
            (40, "Agents  6 · 1 working · 2 done"),
            (21, "Agents  6 · 1 working"),
            (20, "Agents  6"),
            (0, "Agents  6"),
        ] {
            assert_eq!(
                text(&summary(&agents, Tones::Ansi, width)),
                shown,
                "{width}"
            );
        }
        let (left, _) = standing(&agents, Tones::Ansi, true, 49);
        assert_eq!(text(&left), "Agents  6 · 1 working · 2 done · 1 failed");
        let (left, _) = standing(&agents, Tones::Ansi, false, 49);
        assert_eq!(text(&left), "Agents  6 · 1 working · 2 done · 1 failed");
        let (left, _) = standing(&agents, Tones::Ansi, true, 48);
        assert_eq!(text(&left), "Agents  6 · 1 working · 2 done");
    }

    #[test]
    fn selection_falls_back_to_the_first_agent() {
        let agents = [
            agent("a", AgentState::Working),
            agent("b", AgentState::Unavailable),
        ];
        assert_eq!(
            selected(&agents, Some("b")).map(|a| a.id.as_str()),
            Some("b")
        );
        assert_eq!(
            selected(&agents, Some("gone")).map(|a| a.id.as_str()),
            Some("a")
        );
        assert_eq!(selected(&agents, None).map(|a| a.id.as_str()), Some("a"));
        assert_eq!(selected(&[], None), None);
    }

    #[test]
    fn a_live_card_reads_its_state_and_model() {
        let agents = [
            agent("a", AgentState::Working),
            agent("b", AgentState::Saved(Some(Ended::Failed))),
        ];
        let lines: Vec<String> = list(&agents, Some("b"), 60, look(Tones::None))
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            lines,
            [
                format!("  {}", copy::LIVE_LIST_SUBTITLE),
                String::new(),
                "    ● Agent a   a ".into(),
                "    working · faux/faux-2".into(),
                String::new(),
                "  ▸ ✗ Agent b   b ".into(),
                "    failed · saved · faux/faux-2".into(),
            ]
        );
    }

    // The last run's words are one paragraph, cut after `LAST_ROWS` rows,
    // and a failure reads in red.
    #[test]
    fn a_live_ledger_shows_the_model_state_file_and_last_words() {
        let mut failed = agent("child-1", AgentState::Saved(Some(Ended::Failed)));
        failed.file = Some("/s/subagents/child.jsonl".into());
        failed.detail = Some("the request\nfailed".into());
        let lines = ledger(&failed, 60, look(Tones::Ansi));
        let rows: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(rows[0], "  Agent child-1");
        assert_eq!(rows[2], "  Id      child-1 ");
        assert_eq!(rows[3], "  Model   faux/faux-2");
        assert_eq!(rows[4], "  State   failed · saved");
        assert_eq!(rows[5], "  File    /s/subagents/child.jsonl");
        assert_eq!(rows[6], "  Last    the request failed");
        assert_eq!(lines[0].spans[1].style.fg, Some(Color::Cyan));
        assert_eq!(lines[4].spans[2].style.fg, Some(Color::Red));
        assert_eq!(lines[6].spans[2].style.fg, Some(Color::Red));
        // Without colour neither the state nor the failure is red, and a
        // run that did not fail never is.
        let plain = ledger(&failed, 60, look(Tones::None));
        assert_eq!(
            (plain[4].spans[2].style.fg, plain[6].spans[2].style.fg),
            (None, None)
        );
        let mut done = failed.clone();
        done.state = AgentState::Idle(Some(Ended::Completed));
        let lit = ledger(&done, 60, look(Tones::Ansi));
        assert_eq!(
            (lit[4].spans[2].style.fg, lit[6].spans[2].style.fg),
            (Some(Color::Green), None)
        );
        assert!(rows[7].starts_with("  Input   Read only"), "{rows:#?}");
        let mut long = agent("child-2", AgentState::Idle(Some(Ended::Completed)));
        long.detail = Some("word ".repeat(400));
        let rows: Vec<String> = ledger(&long, 40, look(Tones::None))
            .iter()
            .map(text)
            .collect();
        let last: Vec<&String> = rows
            .iter()
            .skip_while(|row| !row.starts_with("  Last"))
            .take_while(|row| !row.starts_with("  Input"))
            .collect();
        assert_eq!(last.len(), LAST_ROWS);
        assert!(last[LAST_ROWS - 1].ends_with('…'));
        // Wrapped rows hang under the value, past the label's column.
        assert!(last[1].starts_with("          word"), "{last:#?}");
        // A cut row that fills its width gives up a cell for the mark.
        long.detail = Some("x".repeat(1000));
        let rows: Vec<String> = ledger(&long, 40, look(Tones::None))
            .iter()
            .map(text)
            .collect();
        let cut = rows
            .iter()
            .filter(|row| row.starts_with("          x") || row.starts_with("  Last"))
            .nth(LAST_ROWS - 1)
            .expect("the last kept row");
        assert_eq!(cut, &format!("{}{}…", " ".repeat(10), "x".repeat(29)));
    }

    // The id's tag follows two cells after the lead only when it fits whole.
    #[test]
    fn a_title_keeps_its_tag_only_when_it_fits() {
        let titled_at = |width| text(&titled(vec![Span::raw("lead")], "id", width, Tones::None));
        assert_eq!(titled_at(10), "lead   id ");
        assert_eq!(titled_at(9), "lead");
    }
}
