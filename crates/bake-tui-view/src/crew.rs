//! The crew panel: the headless agents the session spawned, drawn directly
//! above the composer while any is reported.
//!
//! One header line names the crew, its shared model when all listed agents
//! run the same one, grouped state counts, how many agents are hidden, and
//! `Ctrl+X view` at the right when it fits. Each listed agent then takes one
//! line: its state mark, callsign, model (when listed agents differ), its
//! current tool and last line or status detail, and its elapsed time.
//!
//! Marks match the transcript and tasks list ([`crate::transcript::mark_of`]),
//! one column wide, with an ASCII fallback under the classic frame:
//!
//! | State | Round | Classic |
//! |---|---|---|
//! | starting / working | `●` | `*` |
//! | asking | `?` | `?` |
//! | failed | `✗` | `x` |
//! | queued | `○` | `~` |
//! | idle | `✓` | `+` |
//! | exited | `-` | `-` |
//!
//! While an agent works its callsign shimmers; without motion
//! ([`Tones::moves`], off under `NO_COLOR`) nothing shimmers and nothing is
//! coloured. At most [`MAX_ROWS`] agents are listed, busy ones first; when
//! every agent is idle or exited only the header row shows.
//!
//! Everything here is pure: the runtime reports rows with their age when
//! sent ([`crate::runtime::RuntimeUpdate::Crew`]), and the view ages them
//! by its own clock.

use std::time::Duration;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity::{self, Hue, Tones};
use crate::copy;
use crate::render::{fit, pad_start};
use crate::tasks;
use crate::transcript::{self, CallState, Look};

/// Agents listed at most; more are counted on the header.
pub const MAX_ROWS: usize = 3;

const MARGIN: usize = 2;
const LEAD: usize = 4;
const GAP: usize = 2;
const NAME_MAX: usize = 16;
const MODEL_MAX: usize = 24;
const MODEL_DETAIL_MIN: usize = 20;
const DETAIL_MIN: usize = 6;
const HINT: &str = "Ctrl+X view";

/// Where an agent is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrewState {
    /// Waiting for a slot to start.
    Queued,
    Starting,
    Working,
    Asking,
    Idle,
    Failed(String),
    Exited(Option<String>),
}

impl CrewState {
    fn moves(&self) -> bool {
        matches!(self, Self::Working)
    }

    fn is_quiet(&self) -> bool {
        matches!(self, Self::Idle | Self::Exited(_))
    }

    /// Busy and attention-needing agents are listed first.
    fn rank(&self) -> u8 {
        match self {
            Self::Asking => 0,
            Self::Working => 1,
            Self::Starting => 2,
            Self::Failed(_) => 3,
            Self::Queued => 4,
            Self::Idle => 5,
            Self::Exited(_) => 6,
        }
    }

    fn word(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
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
    /// Its current model as `provider/id`, when known.
    pub model: Option<String>,
    /// The tool call it is in.
    pub tool: Option<String>,
    /// The last line it wrote.
    pub line: Option<String>,
    /// Its task, on one line.
    pub task: String,
    /// How long it had been in its state when reported.
    pub age: Duration,
}

/// How many agent rows the panel lists for `crew`: none when empty or when
/// every agent is idle or exited, else up to [`MAX_ROWS`].
fn listed_count(crew: &[CrewMember]) -> usize {
    if crew.is_empty() || crew.iter().all(|member| member.state.is_quiet()) {
        0
    } else {
        crew.len().min(MAX_ROWS)
    }
}

/// The rows the panel takes for `crew`: none when empty, one header row
/// when every agent is idle or exited, else a header and up to
/// [`MAX_ROWS`] agents.
pub fn rows(crew: &[CrewMember]) -> u16 {
    if crew.is_empty() {
        0
    } else {
        u16::try_from(listed_count(crew) + 1).unwrap_or(0)
    }
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// `colour` as a foreground, or none under `NO_COLOR`.
fn fg(colour: Color, tones: Tones) -> Style {
    match tones {
        Tones::None => Style::new(),
        _ => Style::new().fg(colour),
    }
}

fn mark(state: &CrewState, look: Look) -> Span<'static> {
    let lit = Look { lit: true, ..look };
    let (glyph, style) = match state {
        CrewState::Starting | CrewState::Working => transcript::mark_of(CallState::Running, lit),
        CrewState::Asking => (
            "?",
            fg(Color::Magenta, look.tones).add_modifier(Modifier::BOLD),
        ),
        CrewState::Failed(_) => transcript::mark_of(CallState::Failed, lit),
        CrewState::Queued => (
            if look.classic { "~" } else { "○" },
            fg(Color::Cyan, look.tones),
        ),
        CrewState::Idle => transcript::mark_of(CallState::Done, lit),
        CrewState::Exited(_) => ("-", dim()),
    };
    Span::styled(glyph, style)
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

/// `text` cut to `cells` and padded with spaces on the right.
fn padded(text: &str, cells: usize) -> String {
    let cut = cut(text, cells);
    let fill = cells.saturating_sub(cut.width());
    format!("{cut}{}", " ".repeat(fill))
}

/// The model id without its provider prefix, sanitized to one line.
fn short_model(model: Option<&str>) -> Option<String> {
    let clean = tasks::one_line(model?);
    if clean.is_empty() {
        return None;
    }
    let id = match clean.split_once('/') {
        Some((_, id)) if !id.is_empty() => id.trim(),
        _ => clean.as_str(),
    };
    (!id.is_empty()).then(|| id.to_owned())
}

/// The agent to open first: the one the panel lists first.
pub fn first(crew: &[CrewMember]) -> Option<u64> {
    ordered(crew).first().map(|member| member.id)
}

/// The agent `step` places from `id` in start order, wrapping, so moving
/// between agents does not jump as their states reorder the panel.
pub fn step(crew: &[CrewMember], id: u64, step: isize) -> Option<u64> {
    let mut ids: Vec<u64> = crew.iter().map(|member| member.id).collect();
    ids.sort_unstable();
    let at = ids.iter().position(|&each| each == id)?;
    let len = isize::try_from(ids.len()).ok()?;
    let next = (isize::try_from(at).ok()? + step).rem_euclid(len);
    ids.get(usize::try_from(next).ok()?).copied()
}

/// One agent's transcript in the tray, at most `height` rows: a header with
/// its mark, callsign, state, and model, a rule counting the rows out of
/// view, then its rows `scroll` rows up from the newest. `rows` is `None`
/// until the runtime has read them. Returns the lines and the most it can
/// scroll.
pub fn transcript(
    member: &CrewMember,
    rows: Option<&[crate::transcript::Row]>,
    width: usize,
    height: usize,
    scroll: usize,
    _now: Duration,
    look: Look,
) -> (Vec<Line<'static>>, usize) {
    let dim = dim();
    let mut header_spans = vec![
        Span::raw("  "),
        mark(&member.state, look),
        Span::raw(" "),
        Span::styled(
            tasks::one_line(&member.name),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", member.state.word()), dim),
    ];
    if let Some(model) = short_model(member.model.as_deref()) {
        let used: usize = header_spans.iter().map(|span| span.content.width()).sum();
        let sep = copy::STATUS_SEP;
        let room = width.saturating_sub(used + sep.width());
        if room >= 4 {
            header_spans.push(Span::styled(format!("{sep}{}", cut(&model, room)), dim));
        }
    }
    let header = Line::from(clip(header_spans, width));
    let mut body: Vec<Line<'static>> = match rows {
        None => vec![Line::styled(format!("  {}", copy::CREW_LOADING), dim)],
        Some([]) => vec![Line::styled(format!("  {}", copy::CREW_EMPTY), dim)],
        Some(rows) => (0..rows.len())
            .flat_map(|index| transcript::present(rows, index, width, look))
            .collect(),
    };
    let room = height.saturating_sub(2);
    let most = body.len().saturating_sub(room);
    let scroll = scroll.min(most);
    let end = body.len() - scroll;
    let start = end.saturating_sub(room);
    let mut counts = Vec::new();
    if start > 0 {
        counts.push(format!("+{start} {}", copy::ABOVE));
    }
    if scroll > 0 {
        counts.push(format!("+{scroll} {}", copy::BELOW));
    }
    let mut lines = vec![
        header,
        crate::jobs::counted_rule(width, &counts.join(copy::STATUS_SEP), look),
    ];
    lines.extend(body.drain(start..end));
    (lines, most)
}

/// The agents in the order they are listed.
fn ordered(crew: &[CrewMember]) -> Vec<&CrewMember> {
    let mut sorted: Vec<&CrewMember> = crew.iter().collect();
    sorted.sort_by_key(|member| (member.state.rank(), member.id));
    sorted
}

struct HeaderCount {
    text: String,
    style: Style,
    quiet: bool,
}

fn header_counts(crew: &[CrewMember], look: Look) -> Vec<HeaderCount> {
    let mut active = 0usize;
    let mut asking = 0usize;
    let mut failed = 0usize;
    let mut queued = 0usize;
    let mut done = 0usize;
    for member in crew {
        match member.state {
            CrewState::Starting | CrewState::Working => active += 1,
            CrewState::Asking => asking += 1,
            CrewState::Failed(_) => failed += 1,
            CrewState::Queued => queued += 1,
            CrewState::Idle | CrewState::Exited(_) => done += 1,
        }
    }
    let entries = [
        (active, "active", Style::new(), false),
        (asking, "asking", fg(Color::Magenta, look.tones), false),
        (failed, "failed", transcript::red(look), false),
        (queued, "queued", dim(), true),
        (done, "done", dim(), true),
    ];
    entries
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, word, style, quiet)| HeaderCount {
            text: format!("{n} {word}"),
            style,
            quiet,
        })
        .collect()
}

/// The header line at `width`, dropping whole parts when narrow rather than
/// clipping mid-word: shared model first, then the key hint, then dim counts,
/// then `+N more`, then remaining counts.
fn header_line(
    crew: &[CrewMember],
    shared_model: Option<&str>,
    hidden: usize,
    width: usize,
    look: Look,
) -> Line<'static> {
    let end = width.saturating_sub(MARGIN);
    let quiet = crew.iter().all(|member| member.state.is_quiet());
    let label_style = if quiet {
        dim()
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let counts = header_counts(crew, look);
    let model = shared_model.map(|m| cut(m, MODEL_MAX));
    let more = (hidden > 0).then(|| format!("+{hidden} more"));
    let loud = counts.iter().filter(|c| !c.quiet).count();

    let mut levels: Vec<(bool, bool, bool, usize)> = Vec::new();
    let has_model = model.is_some();
    let has_more = more.is_some();
    let total = counts.len();
    if has_model {
        levels.push((true, true, has_more, total));
        levels.push((true, false, has_more, total));
    }
    levels.push((false, true, has_more, total));
    levels.push((false, false, has_more, total));
    for kept in (loud..total).rev() {
        levels.push((false, false, has_more, kept));
    }
    if has_more {
        levels.push((false, false, false, loud.min(total)));
    }
    for kept in (0..loud.min(total)).rev() {
        levels.push((false, false, false, kept));
    }

    for (keep_model, keep_hint, keep_more, kept_counts) in levels {
        let mut spans = vec![Span::raw("  "), Span::styled("crew", label_style)];
        if keep_model && let Some(model) = &model {
            spans.push(Span::styled(format!("{}{model}", copy::STATUS_SEP), dim()));
        }
        for (i, count) in counts.iter().take(kept_counts).enumerate() {
            if i == 0 {
                spans.push(Span::raw("  "));
            } else {
                spans.push(Span::styled(copy::STATUS_SEP, dim()));
            }
            spans.push(Span::styled(count.text.clone(), count.style));
        }
        if keep_more && let Some(more) = &more {
            spans.push(Span::styled(format!("  {more}"), dim()));
        }
        let used: usize = spans.iter().map(|span| span.content.width()).sum();
        if keep_hint {
            if used + GAP + HINT.len() <= end {
                spans.push(Span::raw(" ".repeat(end - used - HINT.len())));
                spans.push(Span::styled(HINT, dim()));
                return Line::from(spans);
            }
        } else if used <= end {
            return Line::from(spans);
        }
    }
    Line::from(vec![Span::raw("  "), Span::styled("crew", label_style)])
}

fn fallback_detail(member: &CrewMember) -> String {
    member
        .line
        .as_deref()
        .map(tasks::one_line)
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| tasks::one_line(&member.task))
}

fn detail_spans(member: &CrewMember, room: usize, look: Look) -> (Vec<Span<'static>>, usize) {
    let text = match &member.state {
        CrewState::Failed(reason) => {
            let reason = tasks::one_line(reason);
            if reason.is_empty() {
                fallback_detail(member)
            } else {
                reason
            }
        }
        CrewState::Exited(Some(reason)) => {
            let reason = tasks::one_line(reason);
            if reason.is_empty() {
                fallback_detail(member)
            } else {
                reason
            }
        }
        _ => fallback_detail(member),
    };
    if member.state == CrewState::Working
        && let Some(tool) = member
            .tool
            .as_deref()
            .map(tasks::one_line)
            .filter(|tool| !tool.is_empty())
    {
        if text.is_empty() {
            let shown = cut(&tool, room);
            let width = shown.width();
            return (vec![Span::styled(shown, dim())], width);
        }
        let sep = copy::STATUS_SEP;
        let prefix_w = tool.width() + sep.width();
        if prefix_w + 4 <= room {
            let body = cut(&text, room - prefix_w);
            let width = prefix_w + body.width();
            return (
                vec![Span::styled(format!("{tool}{sep}"), dim()), Span::raw(body)],
                width,
            );
        }
        let shown = cut(&tool, room);
        let width = shown.width();
        return (vec![Span::styled(shown, dim())], width);
    }
    let style = match member.state {
        CrewState::Failed(_) => transcript::red(look),
        CrewState::Asking => fg(Color::Magenta, look.tones),
        CrewState::Working | CrewState::Starting => Style::new(),
        CrewState::Queued | CrewState::Idle | CrewState::Exited(_) => dim(),
    };
    let shown = cut(&text, room);
    let width = shown.width();
    (vec![Span::styled(shown, style)], width)
}

/// The panel's lines at `width`, `elapsed` after the rows were reported.
pub fn lines(
    crew: &[CrewMember],
    width: usize,
    now: Duration,
    elapsed: Duration,
    look: Look,
) -> Vec<Line<'static>> {
    if crew.is_empty() || width < 12 {
        return Vec::new();
    }
    let count = listed_count(crew);
    let shown: Vec<&CrewMember> = ordered(crew).into_iter().take(count).collect();
    let hidden = if count > 0 {
        crew.len().saturating_sub(count)
    } else {
        0
    };

    let model_source: Vec<&CrewMember> = if shown.is_empty() {
        crew.iter().collect()
    } else {
        shown.clone()
    };
    let shared_model: Option<String> = model_source
        .first()
        .and_then(|member| short_model(member.model.as_deref()))
        .filter(|first| {
            model_source.iter().all(|member| {
                short_model(member.model.as_deref()).as_deref() == Some(first.as_str())
            })
        });

    let mut out = vec![header_line(
        crew,
        shared_model.as_deref(),
        hidden,
        width,
        look,
    )];
    if shown.is_empty() {
        return out;
    }

    let end = width.saturating_sub(MARGIN);
    let names: Vec<String> = shown
        .iter()
        .map(|member| tasks::one_line(&member.name))
        .collect();
    let ages: Vec<String> = shown
        .iter()
        .map(|member| activity::format_elapsed(member.age + elapsed))
        .collect();
    let models: Vec<Option<String>> = if shared_model.is_some() {
        vec![None; shown.len()]
    } else {
        shown
            .iter()
            .map(|member| short_model(member.model.as_deref()))
            .collect()
    };

    let age_width = ages.iter().map(|age| age.width()).max().unwrap_or(0);
    let show_age = age_width > 0 && end >= LEAD + 4 + GAP + age_width;
    let right_used = if show_age { GAP + age_width } else { 0 };
    let avail = end.saturating_sub(LEAD + right_used);

    let name_full = names
        .iter()
        .map(|name| name.width())
        .max()
        .unwrap_or(0)
        .clamp(1, NAME_MAX);
    let mut name_width = name_full.min(avail);
    if avail >= 12 {
        name_width = name_width.min((avail / 2).max(6));
    }
    let rest = avail.saturating_sub(name_width);
    let model_full = models
        .iter()
        .flatten()
        .map(|model| model.width())
        .max()
        .unwrap_or(0)
        .min(MODEL_MAX);
    let model_width =
        (model_full > 0 && rest >= GAP + model_full + GAP + MODEL_DETAIL_MIN).then_some(model_full);
    let after_model = match model_width {
        Some(mw) => rest.saturating_sub(GAP + mw),
        None => rest,
    };
    let detail_width = (after_model >= GAP + DETAIL_MIN).then(|| after_model - GAP);
    if detail_width.is_none() && model_width.is_none() {
        name_width = name_full.min(avail);
    }

    for (i, member) in shown.into_iter().enumerate() {
        let mut spans = vec![Span::raw("  "), mark(&member.state, look), Span::raw(" ")];
        let name = cut(&names[i], name_width);
        let name_pad = name_width.saturating_sub(name.width());
        if member.state == CrewState::Working {
            spans.extend(activity::shimmer_spans(
                &name,
                now,
                look.tones,
                Hue::Running,
                look.tones.moves(),
            ));
        } else {
            let style = match member.state {
                CrewState::Exited(_) => dim(),
                _ => Style::new().add_modifier(Modifier::BOLD),
            };
            spans.push(Span::styled(name, style));
        }
        if name_pad > 0 {
            spans.push(Span::raw(" ".repeat(name_pad)));
        }
        if let Some(mw) = model_width {
            spans.push(Span::raw(" ".repeat(GAP)));
            let text = models[i].as_deref().unwrap_or("");
            spans.push(Span::styled(padded(text, mw), dim()));
        }
        if let Some(dw) = detail_width {
            spans.push(Span::raw(" ".repeat(GAP)));
            let (detail, used) = detail_spans(member, dw, look);
            spans.extend(detail);
            if show_age {
                let pad = dw.saturating_sub(used);
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                }
            }
        } else if show_age {
            let used_so_far = name_width + model_width.map_or(0, |mw| GAP + mw);
            let pad = avail.saturating_sub(used_so_far);
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
        }
        if show_age {
            spans.push(Span::raw(" ".repeat(GAP)));
            spans.push(Span::styled(pad_start(&ages[i], age_width), dim()));
        }
        out.push(Line::from(clip(spans, width)));
    }
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

/// How long after `now` the panel next changes: the next shimmer beat
/// while an agent works and motion is on, else the next second of its
/// clocks. `None` when empty.
pub fn next_change(crew: &[CrewMember], now: Duration, tones: Tones) -> Option<Duration> {
    if crew.is_empty() {
        return None;
    }
    let moving = tones.moves() && crew.iter().any(|member| member.state.moves());
    Some(activity::next_change(now, moving).max(Duration::from_millis(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: u64, name: &str, state: CrewState) -> CrewMember {
        CrewMember {
            id,
            name: name.into(),
            state,
            model: None,
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

    const PLAIN: Look = Look {
        tones: Tones::None,
        classic: false,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };

    const ANSI: Look = Look {
        tones: Tones::Ansi,
        classic: false,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };

    const CLASSIC: Look = Look {
        tones: Tones::None,
        classic: true,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };

    #[test]
    fn marks_match_transcript_and_stay_one_column_wide() {
        let states = [
            CrewState::Working,
            CrewState::Asking,
            CrewState::Failed("boom".into()),
            CrewState::Queued,
            CrewState::Idle,
            CrewState::Exited(None),
        ];
        for classic in [false, true] {
            let look = Look {
                tones: Tones::None,
                classic,
                lit: true,
                mermaid: crate::mermaid::Mermaid::Streaming,
            };
            let glyphs: Vec<String> = states
                .iter()
                .map(|state| mark(state, look).content.into_owned())
                .collect();
            let expected = if classic {
                ["*", "?", "x", "~", "+", "-"]
            } else {
                ["●", "?", "✗", "○", "✓", "-"]
            };
            assert_eq!(glyphs, expected, "classic {classic}");
            for glyph in &glyphs {
                assert_eq!(glyph.width(), 1, "{glyph}");
            }
        }
        let lit = Look {
            tones: Tones::TrueColor,
            classic: false,
            lit: true,
            mermaid: crate::mermaid::Mermaid::Streaming,
        };
        for (state, call) in [
            (CrewState::Working, CallState::Running),
            (CrewState::Starting, CallState::Running),
            (CrewState::Idle, CallState::Done),
            (CrewState::Failed("x".into()), CallState::Failed),
        ] {
            let expected = transcript::mark_of(call, lit);
            let actual = mark(&state, lit);
            assert_eq!((actual.content.as_ref(), actual.style), expected);
        }
    }

    #[test]
    fn a_transcript_shows_its_newest_rows_and_counts_the_rest() {
        use crate::transcript::Row;
        let mut agent = member(1, "alpha-1", CrewState::Idle);
        agent.model = Some("cliproxyapi/gemini-4-neon-xhigh".into());
        let draw = |rows: Option<&[Row]>, height, scroll| {
            let (lines, most) = transcript(&agent, rows, 60, height, scroll, Duration::ZERO, PLAIN);
            (lines.iter().map(text).collect::<Vec<_>>(), most)
        };
        // Unread, then read but empty.
        let (unread, most) = draw(None, 10, 0);
        assert_eq!(unread[0], "  ✓ alpha-1  idle · gemini-4-neon-xhigh");
        assert_eq!(unread[2], "  Reading its session…");
        assert_eq!(most, 0);
        assert_eq!(draw(Some(&[]), 10, 0).0[2], "  Nothing in its session yet.");

        let rows: Vec<Row> = (1..=6)
            .map(|n| Row::Answer(format!("answer {n}")))
            .collect();
        let (all, most) = draw(Some(&rows), 100, 0);
        let body = all.len() - 2;
        assert_eq!(most, 0);
        assert!(all.iter().any(|line| line.contains("answer 6")), "{all:?}");
        // A short tray keeps the newest rows and counts the rest above.
        let (newest, most) = draw(Some(&rows), 6, 0);
        assert_eq!(newest.len(), 6);
        assert_eq!(most, body - 4);
        assert!(newest[1].contains(&format!("+{most} above")), "{newest:?}");
        assert!(newest.last().unwrap().contains("answer 6"), "{newest:?}");
        // Scrolled up, it counts below too; past the most it stops there.
        let (up, _) = draw(Some(&rows), 6, 2);
        assert!(up[1].contains("+2 below"), "{up:?}");
        assert_eq!(draw(Some(&rows), 6, 999).0, draw(Some(&rows), 6, most).0);
        // Untrusted callsign and model are drawn on one line within width.
        let mut named = member(2, "bad\nname\u{1b}[31m", CrewState::Working);
        named.model = Some("prov/evil\u{1b}[2J\nmodel-very-long-name-here".into());
        let (lines, _) = transcript(&named, None, 40, 6, 0, Duration::ZERO, PLAIN);
        let head = text(&lines[0]);
        assert!(!head.chars().any(char::is_control), "{head:?}");
        assert!(head.width() <= 40, "{head:?}");
    }

    #[test]
    fn moving_between_agents_follows_start_order_and_wraps() {
        let crew = vec![
            member(3, "c", CrewState::Working),
            member(1, "a", CrewState::Idle),
            member(2, "b", CrewState::Asking),
        ];
        // The first opened is the one the panel lists first.
        assert_eq!(first(&crew), Some(2));
        assert_eq!(step(&crew, 2, 1), Some(3));
        assert_eq!(step(&crew, 3, 1), Some(1));
        assert_eq!(step(&crew, 1, -1), Some(3));
        assert_eq!(step(&crew, 9, 1), None);
        assert_eq!(first(&[]), None);
    }

    #[test]
    fn the_panel_lists_busy_agents_first_at_sixty_columns() {
        let mut working = member(2, "alpha-2", CrewState::Working);
        working.tool = Some("bash".into());
        let crew = vec![
            member(1, "alpha-1", CrewState::Idle),
            working,
            member(3, "alpha-3", CrewState::Asking),
        ];
        assert_eq!(rows(&crew), 4);
        let rendered = lines(&crew, 60, Duration::ZERO, Duration::from_secs(60), ANSI);
        let texts: Vec<String> = rendered.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                "  crew  1 active · 1 asking · 1 done           Ctrl+X view",
                "  ? alpha-3  running the tests                      1m 05s",
                "  ● alpha-2  bash · running the tests               1m 05s",
                "  ✓ alpha-1  running the tests                      1m 05s",
            ]
        );
        // Under classic frame, every mark falls back to ASCII.
        let classic: Vec<String> =
            lines(&crew, 60, Duration::ZERO, Duration::from_secs(60), CLASSIC)
                .iter()
                .map(text)
                .collect();
        assert_eq!(
            classic[1..],
            [
                "  ? alpha-3  running the tests                      1m 05s",
                "  * alpha-2  bash · running the tests               1m 05s",
                "  + alpha-1  running the tests                      1m 05s",
            ]
        );
    }

    #[test]
    fn shared_model_shows_on_header_and_mixed_models_show_per_row() {
        let mut a1 = member(1, "alpha-1", CrewState::Working);
        a1.tool = Some("bash".into());
        a1.model = Some("cliproxyapi/gemini-4-neon-xhigh".into());
        let mut a2 = member(2, "alpha-2", CrewState::Asking);
        a2.line = Some("Should I touch retry?".into());
        a2.model = Some("cliproxyapi/gemini-4-neon-xhigh".into());
        let shared = vec![a1.clone(), a2.clone()];
        let texts: Vec<String> = lines(&shared, 80, Duration::ZERO, Duration::ZERO, PLAIN)
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            texts,
            [
                "  crew · gemini-4-neon-xhigh  1 active · 1 asking                  Ctrl+X view",
                "  ? alpha-2  Should I touch retry?                                          5s",
                "  ● alpha-1  bash · running the tests                                       5s",
            ]
        );

        // When models differ, each listed row gets a dim model column.
        let mut a3 = member(3, "bravo-1", CrewState::Failed("refused".into()));
        a3.model = None;
        a1.model = Some("anthropic/claude-sonnet-4".into());
        let mixed = vec![a1, a2, a3];
        let wide: Vec<String> = lines(&mixed, 90, Duration::ZERO, Duration::ZERO, PLAIN)
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            wide,
            [
                "  crew  1 active · 1 asking · 1 failed                                       Ctrl+X view",
                "  ? alpha-2  gemini-4-neon-xhigh  Should I touch retry?                               5s",
                "  ● alpha-1  claude-sonnet-4      bash · running the tests                            5s",
                "  ✗ bravo-1                       refused                                             5s",
            ]
        );
        // Narrow: the model column drops first so details stay readable.
        let narrow: Vec<String> = lines(&mixed, 56, Duration::ZERO, Duration::ZERO, PLAIN)
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            narrow,
            [
                "  crew  1 active · 1 asking · 1 failed     Ctrl+X view",
                "  ? alpha-2  Should I touch retry?                  5s",
                "  ● alpha-1  bash · running the tests               5s",
                "  ✗ bravo-1  refused                                5s",
            ]
        );
    }

    #[test]
    fn narrow_widths_drop_whole_header_parts_without_clipping() {
        let mut a1 = member(1, "alpha-1", CrewState::Working);
        a1.tool = Some("bash".into());
        a1.model = Some("cliproxyapi/gemini-4-neon-xhigh".into());
        let mut a2 = member(2, "alpha-2", CrewState::Asking);
        a2.model = Some("cliproxyapi/gemini-4-neon-xhigh".into());
        let mut a3 = member(3, "alpha-3", CrewState::Failed("boom".into()));
        a3.model = Some("cliproxyapi/gemini-4-neon-xhigh".into());
        let mut a4 = member(4, "alpha-4", CrewState::Queued);
        a4.model = Some("cliproxyapi/gemini-4-neon-xhigh".into());
        let crew = vec![a1, a2, a3, a4];

        let head = |w| text(&lines(&crew, w, Duration::ZERO, Duration::ZERO, PLAIN)[0]);
        assert_eq!(
            head(60),
            "  crew  1 active · 1 asking · 1 failed · 1 queued  +1 more"
        );
        assert_eq!(head(44), "  crew  1 active · 1 asking · 1 failed");
        assert_eq!(head(30), "  crew  1 active · 1 asking");
        assert_eq!(head(18), "  crew  1 active");
        assert_eq!(head(12), "  crew");

        // Narrow row drops detail cleanly and stays within 24 columns.
        let at_24: Vec<String> = lines(&crew, 24, Duration::ZERO, Duration::ZERO, PLAIN)
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            at_24,
            [
                "  crew  1 active",
                "  ? alpha-2         5s",
                "  ● alpha-1         5s",
                "  ✗ alpha-3         5s",
            ]
        );
    }

    #[test]
    fn all_idle_or_exited_crew_shows_only_the_header() {
        let crew = vec![
            member(1, "alpha-1", CrewState::Idle),
            member(2, "alpha-2", CrewState::Exited(None)),
        ];
        assert_eq!(rows(&crew), 1);
        let texts: Vec<String> = lines(&crew, 60, Duration::ZERO, Duration::ZERO, PLAIN)
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            texts,
            ["  crew  2 done                                 Ctrl+X view"]
        );
    }

    #[test]
    fn a_queued_agent_shows_its_task_and_does_not_move() {
        let mut queued = member(2, "alpha-2", CrewState::Queued);
        queued.line = None;
        let crew = vec![member(1, "alpha-1", CrewState::Idle), queued];
        let texts: Vec<String> = lines(&crew, 60, Duration::ZERO, Duration::ZERO, PLAIN)
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            texts,
            [
                "  crew  1 queued · 1 done                      Ctrl+X view",
                "  ○ alpha-2  write tests                                5s",
                "  ✓ alpha-1  running the tests                          5s",
            ]
        );
        // It waits still, so nothing moves.
        assert_eq!(
            next_change(&crew[1..], Duration::from_millis(400), Tones::Ansi),
            Some(Duration::from_millis(600))
        );
    }

    #[test]
    fn shimmer_moves_only_with_motion_and_working_agents() {
        let crew = vec![member(1, "alpha-1", CrewState::Working)];
        // Under NO_COLOR, no spans are coloured and nothing moves.
        let plain_a = lines(&crew, 40, Duration::ZERO, Duration::ZERO, PLAIN);
        let plain_b = lines(&crew, 40, Duration::from_millis(210), Duration::ZERO, PLAIN);
        assert_eq!(plain_a, plain_b);
        assert_eq!(
            next_change(&crew, Duration::from_millis(50), Tones::Ansi),
            Some(Duration::from_millis(20))
        );
        assert_eq!(
            next_change(&crew, Duration::from_millis(50), Tones::None),
            Some(Duration::from_millis(950))
        );
        assert_eq!(next_change(&[], Duration::ZERO, Tones::Ansi), None);
        let idle = vec![member(1, "a", CrewState::Idle)];
        assert_eq!(
            next_change(&idle, Duration::from_millis(400), Tones::Ansi),
            Some(Duration::from_millis(600))
        );
    }

    #[test]
    fn rows_matches_lines_len_across_states_and_widths() {
        let cases = [
            vec![],
            vec![member(1, "a", CrewState::Idle)],
            vec![
                member(1, "a", CrewState::Idle),
                member(2, "b", CrewState::Exited(None)),
            ],
            vec![member(1, "a", CrewState::Working)],
            vec![
                member(1, "a", CrewState::Working),
                member(2, "b", CrewState::Idle),
            ],
            vec![
                member(1, "a", CrewState::Asking),
                member(2, "b", CrewState::Idle),
                member(3, "c", CrewState::Exited(None)),
            ],
            vec![
                member(1, "a", CrewState::Asking),
                member(2, "b", CrewState::Working),
                member(3, "c", CrewState::Starting),
                member(4, "d", CrewState::Failed("err".into())),
                member(5, "e", CrewState::Queued),
                member(6, "f", CrewState::Idle),
                member(7, "g", CrewState::Exited(None)),
            ],
        ];
        for crew in &cases {
            for width in [12usize, 16, 24, 40, 60, 100, 160] {
                let rendered = lines(crew, width, Duration::ZERO, Duration::ZERO, PLAIN);
                assert_eq!(
                    usize::from(rows(crew)),
                    rendered.len(),
                    "width {width}, crew len {}",
                    crew.len()
                );
                for line in &rendered {
                    assert!(text(line).width() <= width, "{width}: {:?}", text(line));
                }
            }
        }
    }

    #[test]
    fn long_crews_and_narrow_screens_stay_in_bounds() {
        let crew: Vec<CrewMember> = (0..7)
            .map(|id| {
                let state = if id == 0 {
                    CrewState::Working
                } else {
                    CrewState::Idle
                };
                let mut m = member(id, &format!("a-very-long-callsign-{id}\u{1b}[31m\n"), state);
                m.model = Some("prov/a-very-long-untrusted-model\u{7}name".into());
                m.line = Some("日本語のテスト\u{1b}[2J\tline".into());
                m
            })
            .collect();
        assert_eq!(rows(&crew), 4);
        for width in [12usize, 20, 33, 80] {
            let rendered = lines(&crew, width, Duration::ZERO, Duration::ZERO, PLAIN);
            assert!(text(&rendered[0]).contains("+4 more") || width < 40);
            for line in &rendered {
                let shown = text(line);
                assert!(shown.width() <= width, "{width}: {shown:?}");
                assert!(!shown.chars().any(char::is_control), "{width}: {shown:?}");
            }
        }
    }
}
