//! Agents: the session's subagents as the runtime reports them, and one
//! agent's read-only ledger. The row under the composer and the list that
//! show them beside the background shells are [`crate::tasks`]'s.
//!
//! A live session reports the subagents it started
//! ([`crate::runtime::RuntimeUpdate::Agents`]), in start order, each with
//! its state, how its last run in this process ended, and what that run
//! said last; the preview's fixture runtime reports samples the same way.
//! The view draws what was reported and asks the runtime to stop a working
//! agent's run ([`crate::state::Effect::StopAgent`]); the runtime reports
//! the change. The words follow the 0.3 terminal's subagent sheet
//! (`apps/tui/packages/ui/src/subagents.tsx`): how the last run ended, then
//! whether the agent is resident.

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::activity::Tones;
use crate::copy;
use crate::tasks;
use crate::transcript::{self, Look, TagTone};

/// One agent as the runtime reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    /// Its id: a subagent's session id.
    pub id: String,
    /// Its label.
    pub name: String,
    pub state: AgentState,
    /// `provider/model`; a preview sample runs on none.
    pub route: Option<String>,
    /// What its last run said: its closing message, or why it failed.
    pub detail: Option<String>,
    /// Its session file, when it keeps one.
    pub file: Option<String>,
}

/// Where an agent is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
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

/// How the agent is doing, outcome first: `starting`, `working`,
/// `done · idle`, `failed · saved`, `saved`, or `unreadable`.
pub fn word(state: AgentState) -> String {
    let rest = match state {
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

/// The word for how a run ended.
pub(crate) fn ended_word(ended: Ended) -> &'static str {
    match ended {
        Ended::Completed => copy::AGENT_DONE,
        Ended::Failed => copy::AGENT_FAILED,
        Ended::Stopped => copy::AGENT_STOPPED,
    }
}

/// The colour of the ledger's state: its mark's colour once a run has
/// ended, or while the file cannot be read; the terminal's foreground
/// otherwise.
fn color(state: AgentState) -> Option<Color> {
    match (state, state.ended()) {
        (AgentState::Unavailable, _) | (_, Some(Ended::Failed)) => Some(Color::Red),
        (_, Some(Ended::Completed)) => Some(Color::Green),
        (_, Some(Ended::Stopped)) => Some(Color::Yellow),
        _ => None,
    }
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
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

/// Cells a ledger's label column takes, its gap included.
const LEDGER_LABEL: usize = 8;

/// Rows of a run's last words the ledger shows before it cuts them.
pub const LAST_ROWS: usize = 8;

/// One agent, read only, as a ledger: its mark and name, as its row in the
/// tasks list shows them, a dashed rule, then a label and a value per row,
/// each value wrapped under itself: its id, model, state, file, and its
/// last run's words. What typing and the draft do meanwhile is the
/// composer's to say, so the ledger does not repeat it. Every value comes
/// from the runtime, so escape sequences and control characters are taken
/// out first.
pub fn ledger(agent: &Agent, width: usize, look: Look) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::raw("  "),
            tasks::mark(tasks::phase_of(agent.state), look),
            Span::raw(" "),
            Span::styled(
                tasks::one_line(&agent.name),
                Style::new().add_modifier(Modifier::BOLD),
            ),
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
        vec![transcript::tag(
            &tasks::one_line(&agent.id),
            TagTone::Blue,
            look.tones,
        )],
    ));
    let text = |label: &str, value: &str, style: Style, rows: usize| -> Vec<Line<'static>> {
        let mut wrapped = transcript::wrap(&tasks::one_line(value), width.saturating_sub(indent));
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
        lines.extend(text(copy::LEDGER_LAST, detail, style, LAST_ROWS));
    }
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
        ];
        for (state, expected) in cases {
            assert_eq!(word(state), expected, "{state:?}");
        }
    }

    // The ledger opens with the task's mark and name, then a rule and one
    // row per value; the last run's words are one paragraph, cut after
    // `LAST_ROWS` rows, and a failure reads in red. Nothing repeats what
    // the composer says about typing and the draft.
    #[test]
    fn a_live_ledger_shows_the_model_state_file_and_last_words() {
        let mut failed = agent("child-1", AgentState::Saved(Some(Ended::Failed)));
        failed.file = Some("/s/subagents/child.jsonl".into());
        failed.detail = Some("the request\nfailed".into());
        let lines = ledger(&failed, 60, look(Tones::Ansi));
        let rows: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            rows,
            [
                "  ✗ Agent child-1".to_owned(),
                format!("  {}", "-".repeat(56)),
                "  Id      child-1 ".into(),
                "  Model   faux/faux-2".into(),
                "  State   failed · saved".into(),
                "  File    /s/subagents/child.jsonl".into(),
                "  Last    the request failed".into(),
            ]
        );
        assert_eq!(lines[0].spans[1].style.fg, Some(Color::Red));
        assert!(
            lines[0].spans[3]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(lines[4].spans[2].style.fg, Some(Color::Red));
        assert_eq!(lines[6].spans[2].style.fg, Some(Color::Red));
        // Without colour neither the state nor the failure is red, and a
        // run that did not fail never is; a working agent's state is plain.
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
        let working = ledger(&agent("w", AgentState::Working), 60, look(Tones::Ansi));
        assert_eq!(text(&working[0]), "  ● Agent w");
        assert_eq!(working[4].spans[2].style.fg, None);
        let mut long = agent("child-2", AgentState::Idle(Some(Ended::Completed)));
        long.detail = Some("word ".repeat(400));
        let rows: Vec<String> = ledger(&long, 40, look(Tones::None))
            .iter()
            .map(text)
            .collect();
        let last: Vec<&String> = rows
            .iter()
            .skip_while(|row| !row.starts_with("  Last"))
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

    // The runtime's words are untrusted: no escape sequence or control
    // character from any of them reaches the terminal.
    #[test]
    fn a_ledger_draws_no_control_character() {
        let mut hostile = agent("id\u{1b}[2J", AgentState::Idle(Some(Ended::Failed)));
        hostile.name = "name\u{1b}]0;title\u{7}".into();
        hostile.route = Some("route\r\u{8}".into());
        hostile.file = Some("/f\u{1b}[31m".into());
        hostile.detail = Some("last\u{1b}[H\u{9b}words".into());
        for width in [0, 8, 40, 80] {
            for line in ledger(&hostile, width, look(Tones::None)) {
                let shown = text(&line);
                assert!(!shown.chars().any(char::is_control), "{shown:?}");
            }
        }
    }
}
