//! The status line's fields and the order they give way in as the row
//! narrows; a port of the TypeScript `status-line.ts` fitting.
//!
//! The row is minimal: `● deepseek-v4-flash high  ctx ▰▱▱▱▱ ~11%
//! (15.2k/128k)  ~/bake ⎇ main`, the session's state dot, the model and
//! its thinking level, the context window's occupancy as a gauge and a
//! count, and the working directory with its git branch. A reading the
//! session does not have is left out. The session's token totals
//! (`↑12k ↓3.4k`) and its prompt cache (`cache 99% (R180k W2k)`), Pi's
//! footer readings, show after the context when the user turns them on.
//!
//! The row is quiet: the model is in the terminal's foreground, everything
//! else is dim, and colour comes from [`crate::palette`] only where it
//! means something. The state dot follows the session ([`SessionState`]),
//! with a distinct glyph for each state so it reads without colour. The
//! gauge and the percentage warm from `ok` through `notice` and `warn` to
//! `danger` as the context fills; the cache's share warms as it falls; a
//! detached branch is a `notice`; the thinking levels take the cool scale.
//!
//! The dot never gives way. The fields keep their order, and each gives
//! way at its own rank, whole or to a shorter complete reading, never cut
//! mid-word, except the two fields that shrink: the model, cut from its
//! end, and the working directory, which fills what the others leave and is
//! cut from its start.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::copy;
use crate::palette::Token;

/// The order fields give way in, first to yield first, in tenths so the
/// TypeScript table's half ranks stay exact. Only some fields exist in the
/// preview; the rest keep their places for the fields that will.
pub mod rank {
    pub const CONTEXT_GAUGE: u16 = 5;
    pub const CONTEXT_ABSOLUTE: u16 = 10;
    pub const TOTALS: u16 = 20;
    pub const CONTEXT_MARK: u16 = 25;
    pub const CWD: u16 = 30;
    pub const UPDATE: u16 = 40;
    pub const GIT_COUNTS: u16 = 50;
    pub const CACHE_HIT: u16 = 60;
    pub const CONTEXT_MARK_DROP: u16 = 65;
    pub const BRANCH: u16 = 70;
    pub const THINKING: u16 = 80;
    pub const CONTEXT_ABSOLUTE_WARM: u16 = 85;
    pub const MODEL: u16 = 90;
    pub const CONTEXT_ABSOLUTE_FULL: u16 = 95;
}

/// Fewest cells the model is cut to. Fewer name no model.
pub const MODEL_MIN: usize = 8;
/// Fewest cells of the working directory worth drawing: a shorter tail such
/// as `…ake` names nothing.
pub const CWD_MIN: usize = 6;
/// Cells between fields. A separator glyph would be another width to measure.
pub const FIELD_GAP: usize = 2;

/// How a run of a field is drawn. The renderer maps a [`Tone::Ink`] to what
/// the terminal can show through [`Token::style`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// A value: dim, like the labels, so colour stands out.
    #[default]
    Plain,
    /// The row's anchor, the model: the terminal's own foreground.
    Anchor,
    /// A label or supporting text: [`Token::Muted`].
    Dim,
    /// A palette colour that means something.
    Ink(Token),
}

/// Context occupancy from here up wants a look: `notice`.
pub const CONTEXT_NOTICE: u64 = 60;
/// Context occupancy from here up keeps its count longer.
pub const CONTEXT_WARN: u64 = 70;
/// Context occupancy from here up nears the compaction point: `warn`.
pub const CONTEXT_HOT: u64 = 80;
/// Context occupancy from here up is close to the model's limit: `danger`.
pub const CONTEXT_FULL: u64 = 90;
/// Cells of the context gauge.
pub const GAUGE_CELLS: usize = 5;
/// A cache share from here up is healthy: `ok`.
pub const CACHE_OK: u64 = 80;
/// A cache share below here is mostly missing: `warn`; between, `notice`.
pub const CACHE_LOW: u64 = 50;
/// Cells the state dot takes with the space after it.
pub const DOT_WIDTH: usize = 2;

/// One run of a status field, in its own tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub tone: Tone,
}

impl Part {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// One complete reading of a field, as its runs.
pub type Form = Vec<Part>;

/// How a shrinking field is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShrinkKind {
    /// Cut from its end at its rank, down to its minimum (the model).
    End,
    /// The row's filler, cut from its start into what the others leave and
    /// drawn while its minimum fits (the working directory).
    Fill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shrink {
    pub kind: ShrinkKind,
    pub rank: u16,
    pub min: usize,
}

/// One status-line field. `forms` run from the widest reading to the
/// narrowest; `yields[i]` is the rank at which form `i` gives way to form
/// `i + 1`, or past the last form to nothing. A form with no rank never yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub forms: Vec<Form>,
    pub yields: Vec<u16>,
    pub shrink: Option<Shrink>,
}

/// A field as it is drawn: the reading kept, its cells, and where it is cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    pub parts: Form,
    pub width: usize,
    pub cut: Option<Cut>,
}

/// Which end of a reading is cut to fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    End,
    Start,
}

/// A reading's cells.
pub fn form_width(form: &[Part]) -> usize {
    form.iter().map(|part| part.text.width()).sum()
}

/// Fits the fields to `room` cells, giving way in rank order until they fit.
///
/// At each rank, while the row is too wide, every field that yields there
/// moves to its next reading or goes. The filler takes only what the others
/// leave, cut from its start and drawn while at least its minimum fits;
/// until its own rank the fields ranked before it give way to keep that
/// minimum for it. A field cut from its end counts at its minimum after its
/// rank and gets back whatever is left. Below the widths every rank leaves,
/// the caller clips the row.
///
/// A filler with a second reading fills only in its first. At its rank it
/// steps to the second, a fixed reading that gives way at its own rank, as
/// the location field steps from `~/bake ⎇ main` to `⎇ main`.
pub fn fit(fields: &[Field], room: usize) -> Vec<Fitted> {
    let mut form = vec![0usize; fields.len()];
    let mut shrunk = vec![false; fields.len()];
    let shown = |form: &[usize], i: usize| form[i] < fields[i].forms.len();
    let full = |form: &[usize], i: usize| form_width(&fields[i].forms[form[i]]);
    let filler = |form: &[usize], i: usize| {
        form[i] == 0 && matches!(fields[i].shrink, Some(s) if s.kind == ShrinkKind::Fill)
    };
    // Cells a field counts for while ranks apply: the filler its minimum
    // until its rank and nothing after it, a field cut from its end its
    // minimum after its rank, and every other field its reading.
    let demand = |form: &[usize], shrunk: &[bool], i: usize| match fields[i].shrink {
        None => full(form, i),
        Some(s) if s.kind == ShrinkKind::Fill && form[i] > 0 => full(form, i),
        Some(s) if s.kind == ShrinkKind::Fill => {
            if shrunk[i] {
                0
            } else {
                full(form, i).min(s.min)
            }
        }
        Some(s) => {
            if shrunk[i] {
                full(form, i).min(s.min)
            } else {
                full(form, i)
            }
        }
    };
    let total = |form: &[usize], shrunk: &[bool]| {
        let counted: Vec<usize> = (0..fields.len())
            .filter(|&i| shown(form, i))
            .map(|i| demand(form, shrunk, i))
            .filter(|&cells| cells > 0)
            .collect();
        counted.iter().sum::<usize>() + counted.len().saturating_sub(1) * FIELD_GAP
    };
    let mut ranks: Vec<u16> = fields
        .iter()
        .flat_map(|f| f.yields.iter().copied().chain(f.shrink.map(|s| s.rank)))
        .collect();
    ranks.sort_unstable();
    ranks.dedup();
    for rank in ranks {
        if total(&form, &shrunk) <= room {
            break;
        }
        for (i, field) in fields.iter().enumerate() {
            if !shown(&form, i) {
                continue;
            }
            if field.yields.get(form[i]) == Some(&rank) {
                form[i] += 1;
            } else if field.shrink.is_some_and(|s| s.rank == rank)
                && form[i] == field.forms.len() - 1
            {
                shrunk[i] = true;
            }
        }
    }
    // Lay out every field but the filler at its reading, give the one cut
    // from its end back what is left, then put the filler in the rest.
    let placed = |i: usize| shown(&form, i) && !filler(&form, i) && demand(&form, &shrunk, i) > 0;
    let count = (0..fields.len()).filter(|&i| placed(i)).count();
    let fixed: usize = (0..fields.len())
        .filter(|&i| placed(i))
        .map(|i| demand(&form, &shrunk, i))
        .sum::<usize>()
        + count.saturating_sub(1) * FIELD_GAP;
    let widths: Vec<usize> = (0..fields.len())
        .map(|i| {
            if !placed(i) {
                0
            } else if shrunk[i] && matches!(fields[i].shrink, Some(s) if s.kind == ShrinkKind::End)
            {
                // The others may already overrun the row; then it gives up cells.
                let given = (demand(&form, &shrunk, i) + room).saturating_sub(fixed);
                full(&form, i).min(given)
            } else {
                demand(&form, &shrunk, i)
            }
        })
        .collect();
    let used = widths.iter().sum::<usize>() + count.saturating_sub(1) * FIELD_GAP;
    let mut fitted = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        if !shown(&form, i) {
            continue;
        }
        let parts = field.forms[form[i]].clone();
        let whole = full(&form, i);
        if filler(&form, i) {
            let gap = if used > 0 { FIELD_GAP } else { 0 };
            let left = room.saturating_sub(used + gap);
            let min = field.shrink.map_or(0, |s| s.min);
            if room >= used + gap && left >= min.min(whole) {
                let width = left.min(whole);
                fitted.push(Fitted {
                    parts,
                    width,
                    cut: (width < whole).then_some(Cut::Start),
                });
            }
            continue;
        }
        let width = widths[i];
        if width == 0 {
            continue;
        }
        fitted.push(Fitted {
            parts,
            width,
            cut: (width < whole).then_some(Cut::End),
        });
    }
    fitted
}

/// The fitted field's runs as drawn: whole, or cut to its width with an
/// ellipsis where the cut is. Each run keeps its tone.
pub fn drawn(field: &Fitted) -> Vec<Part> {
    let Some(cut) = field.cut else {
        return field.parts.clone();
    };
    let keep = field.width.saturating_sub(1);
    let graphemes: Vec<(Tone, &str)> = field
        .parts
        .iter()
        .flat_map(|part| part.text.graphemes(true).map(move |g| (part.tone, g)))
        .collect();
    let mut kept: Vec<(Tone, &str)> = Vec::new();
    let mut cells = 0;
    let ordered: Box<dyn Iterator<Item = &(Tone, &str)>> = match cut {
        Cut::End => Box::new(graphemes.iter()),
        Cut::Start => Box::new(graphemes.iter().rev()),
    };
    for &(tone, g) in ordered {
        let w = g.width();
        if cells + w > keep {
            break;
        }
        cells += w;
        kept.push((tone, g));
    }
    if cut == Cut::Start {
        kept.reverse();
    }
    let ellipsis_tone = match cut {
        Cut::End => kept.last().map_or(Tone::Dim, |&(tone, _)| tone),
        Cut::Start => kept.first().map_or(Tone::Dim, |&(tone, _)| tone),
    };
    let mut runs: Vec<Part> = Vec::new();
    let mut push = |tone: Tone, text: &str| match runs.last_mut() {
        Some(last) if last.tone == tone => last.text.push_str(text),
        _ => runs.push(Part::new(text, tone)),
    };
    if cut == Cut::Start {
        push(ellipsis_tone, "…");
    }
    for (tone, g) in kept {
        push(tone, g);
    }
    if cut == Cut::End {
        push(ellipsis_tone, "…");
    }
    runs
}

/// `cwd` as the status line reads it: under `home`, with `~` in its place.
pub fn home_relative(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home
        .map(|h| h.trim_end_matches(['/', '\\']))
        .filter(|h| !h.is_empty())
    else {
        return cwd.to_owned();
    };
    match cwd.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with(['/', '\\']) => format!("~{rest}"),
        _ => cwd.to_owned(),
    }
}

/// How full the context window is, in tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsage {
    /// Estimated tokens in the next request; `None` while unknown, as after
    /// a compaction until the next response measures the context.
    pub used: Option<u64>,
    /// The model's context capacity.
    pub window: u64,
}

/// The checked-out branch, or the abbreviated commit when HEAD is detached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub detached: bool,
}

/// The session's token totals, Pi's footer `↑input ↓output`: uncached
/// input and output over every response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
}

/// The session's prompt cache, Pi's footer `R`, `W`, and `CH`: tokens
/// read from and written to the cache over every response, and the share
/// of the latest response's prompt read from it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheReading {
    pub read: u64,
    pub write: u64,
    /// Whole percent, rounded down; `None` before a response with a prompt.
    pub hit: Option<u64>,
}

/// What the status line reports. Each reading is optional except the
/// directory, which is empty when unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusInput {
    /// `provider/model`, or a bare model name; the provider is left out.
    /// Absent, the row says no model is selected.
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub context: Option<ContextUsage>,
    /// Shown when the user turns them on (`statusLine.tokens`).
    pub tokens: Option<TokenTotals>,
    /// Shown when the user turns it on (`statusLine.cache`).
    pub cache: Option<CacheReading>,
    pub branch: Option<Branch>,
    /// The working directory, already shortened against home.
    pub cwd: String,
    /// Whether the terminal draws the classic frame, which also drops the
    /// branch glyph.
    pub ascii: bool,
}

/// The model name without its provider.
pub fn compact_model(route: &str) -> &str {
    route.rsplit('/').next().unwrap_or(route)
}

/// Whole-percent occupancy, rounded down so a context close to full never
/// reads as 100%; 0 while the tokens are unknown.
pub fn context_percent(usage: ContextUsage) -> u64 {
    usage
        .used
        .unwrap_or(0)
        .saturating_mul(100)
        .checked_div(usage.window)
        .unwrap_or(0)
}

/// A token count as the status line abbreviates it: `950`, `15.2k`, `1M`.
pub fn format_tokens(tokens: u64) -> String {
    fn trim(value: f64) -> String {
        let fixed = format!("{value:.1}");
        fixed
            .strip_suffix(".0")
            .map_or(fixed.clone(), str::to_owned)
    }
    if tokens < 1_000 {
        return tokens.to_string();
    }
    // Promote on the rounded magnitude, so 999,999 reads `1M`, not `1000k`.
    let thousands = tokens as f64 / 1_000.0;
    if thousands < 999.95 {
        format!("{}k", trim(thousands))
    } else {
        format!("{}M", trim(tokens as f64 / 1_000_000.0))
    }
}

/// The context percentage's tone: plain while there is plenty of room,
/// then `notice`, `warn`, and `danger` as it fills.
pub fn context_tone(percent: u64) -> Tone {
    if percent >= CONTEXT_FULL {
        Tone::Ink(Token::Danger)
    } else if percent >= CONTEXT_HOT {
        Tone::Ink(Token::Warn)
    } else if percent >= CONTEXT_NOTICE {
        Tone::Ink(Token::Notice)
    } else {
        Tone::Plain
    }
}

/// The gauge's filled cells: `ok` while there is plenty of room, then the
/// percentage's tone.
pub fn gauge_tone(percent: u64) -> Tone {
    match context_tone(percent) {
        Tone::Plain => Tone::Ink(Token::Ok),
        tone => tone,
    }
}

/// Filled cells of the gauge, to the nearest: the gauge is full from 90%,
/// where the context turns `danger`.
pub fn gauge_filled(percent: u64) -> usize {
    let cells = GAUGE_CELLS as u64;
    ((percent.min(100) * cells + 50) / 100) as usize
}

/// The cache share's tone: `ok` while most of the prompt is read from the
/// cache, `notice` from half, and `warn` below.
pub fn cache_tone(hit: u64) -> Tone {
    Tone::Ink(if hit >= CACHE_OK {
        Token::Ok
    } else if hit >= CACHE_LOW {
        Token::Notice
    } else {
        Token::Warn
    })
}

/// A thinking level's tone, cooling as it asks for more, so a level never
/// reads as a warning: the light efforts dim, `medium` plain, `high` sky,
/// `xhigh` violet, and `max` pink.
pub fn thinking_tone(level: &str) -> Tone {
    match level.to_lowercase().as_str() {
        "none" | "off" | "minimal" | "low" => Tone::Dim,
        "high" => Tone::Ink(Token::High),
        "xhigh" => Tone::Ink(Token::XHigh),
        "max" => Tone::Ink(Token::Max),
        _ => Tone::Plain,
    }
}

/// What the session is doing, which the state dot leading the row shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing runs and nothing waits.
    #[default]
    Idle,
    /// A turn or a compaction runs.
    Working,
    /// Waiting on the user: an approval, or a model to select.
    Waiting,
    /// The last turn failed; it holds until the next prompt.
    Failed,
}

/// The state dot: a glyph for each state, so they differ without colour,
/// in the state's tone. The classic frame spells them in ASCII.
pub fn dot(state: SessionState, ascii: bool) -> Part {
    let (glyph, ascii_glyph, tone) = match state {
        SessionState::Idle => ("○", "o", Tone::Dim),
        SessionState::Working => ("●", "*", Tone::Ink(Token::Accent)),
        SessionState::Waiting => ("◆", "?", Tone::Ink(Token::Notice)),
        SessionState::Failed => ("!", "!", Tone::Ink(Token::Danger)),
    };
    Part::new(if ascii { ascii_glyph } else { glyph }, tone)
}

/// The status line as drawn in `room` cells: the state dot, which never
/// gives way, a space, and the fields fitted to the rest, two cells apart.
/// A field the rest cannot hold whole at its narrowest goes whole, from the
/// end, rather than be cut.
pub fn line(input: &StatusInput, state: SessionState, room: usize) -> Vec<Part> {
    if room == 0 {
        return Vec::new();
    }
    let mut parts = vec![dot(state, input.ascii)];
    let rest = room.saturating_sub(DOT_WIDTH);
    let mut fitted = fit(&fields(input), rest);
    while !fitted.is_empty() && fitted_width(&fitted) > rest {
        fitted.pop();
    }
    for (i, field) in fitted.iter().enumerate() {
        let gap = if i == 0 { DOT_WIDTH - 1 } else { FIELD_GAP };
        parts.push(Part::new(" ".repeat(gap), Tone::Plain));
        parts.extend(drawn(field));
    }
    parts
}

/// The status line's fields, in display order: the model with its thinking
/// level, the context reading, the token totals and the cache when they
/// are on, and the location, which is the working directory with the
/// branch after it.
pub fn fields(input: &StatusInput) -> Vec<Field> {
    let mut fields = vec![model_field(input)];
    if let Some(usage) = input.context {
        fields.push(context_field(usage, input.ascii));
    }
    if let Some(totals) = input.tokens {
        fields.push(tokens_field(totals, input.ascii));
    }
    if let Some(field) = input.cache.and_then(cache_field) {
        fields.push(field);
    }
    if let Some(field) = location_field(input) {
        fields.push(field);
    }
    fields
}

/// `deepseek-v4-flash high`: the level reads as the model's, so it needs no
/// label. It gives way first, and then the model is cut from its end.
fn model_field(input: &StatusInput) -> Field {
    let Some(model) = &input.model else {
        return Field {
            forms: vec![vec![Part::new(
                copy::NO_MODEL_FIELD,
                Tone::Ink(Token::Notice),
            )]],
            yields: Vec::new(),
            shrink: None,
        };
    };
    let name = Part::new(compact_model(model), Tone::Anchor);
    let mut forms = Vec::new();
    let mut yields = Vec::new();
    if let Some(level) = &input.thinking {
        forms.push(vec![
            name.clone(),
            Part::new(format!(" {level}"), thinking_tone(level)),
        ]);
        yields.push(rank::THINKING);
    }
    forms.push(vec![name]);
    Field {
        forms,
        yields,
        shrink: Some(Shrink {
            kind: ShrinkKind::End,
            rank: rank::MODEL,
            min: MODEL_MIN,
        }),
    }
}

/// `ctx ▰▱▱▱▱ ~11% (15.2k/128k)`, or `ctx [#----] ~11% (15.2k/128k)` in
/// the classic frame. The gauge goes first, then the absolute count, except
/// near the limit, where the count is among the last; the percentage never
/// yields. Unknown tokens read `ctx ?/128k`, as Pi's footer has them, and
/// then `ctx ?`, with no gauge.
fn context_field(usage: ContextUsage, ascii: bool) -> Field {
    let label = Part::new(format!("{} ", copy::CONTEXT), Tone::Dim);
    let Some(used) = usage.used else {
        return Field {
            forms: vec![
                vec![
                    label.clone(),
                    Part::new(format!("?/{}", format_tokens(usage.window)), Tone::Plain),
                ],
                vec![label, Part::new("?", Tone::Plain)],
            ],
            yields: vec![rank::CONTEXT_ABSOLUTE],
            shrink: None,
        };
    };
    let percent = context_percent(usage);
    let tone = context_tone(percent);
    let absolute = format!(
        "~{percent}% ({}/{})",
        format_tokens(used),
        format_tokens(usage.window)
    );
    let absolute_rank = if percent >= CONTEXT_FULL {
        rank::CONTEXT_ABSOLUTE_FULL
    } else if percent >= CONTEXT_WARN {
        rank::CONTEXT_ABSOLUTE_WARM
    } else {
        rank::CONTEXT_ABSOLUTE
    };
    let mut gauged = vec![label.clone()];
    gauged.extend(gauge(percent, ascii));
    gauged.push(Part::new(format!(" {absolute}"), tone));
    Field {
        forms: vec![
            gauged,
            vec![label.clone(), Part::new(absolute, tone)],
            vec![label, Part::new(format!("~{percent}%"), tone)],
        ],
        yields: vec![rank::CONTEXT_GAUGE, absolute_rank],
        shrink: None,
    }
}

/// The context gauge: [`GAUGE_CELLS`] cells, the filled ones in the
/// occupancy's tone and the rest dim.
fn gauge(percent: u64, ascii: bool) -> Vec<Part> {
    let filled = gauge_filled(percent);
    let (on, off) = if ascii { ("#", "-") } else { ("▰", "▱") };
    let mut parts = Vec::new();
    if ascii {
        parts.push(Part::new("[", Tone::Dim));
    }
    if filled > 0 {
        parts.push(Part::new(on.repeat(filled), gauge_tone(percent)));
    }
    if filled < GAUGE_CELLS {
        parts.push(Part::new(off.repeat(GAUGE_CELLS - filled), Tone::Dim));
    }
    if ascii {
        parts.push(Part::new("]", Tone::Dim));
    }
    parts
}

/// `↑12k ↓3.4k`, or `in 12k out 3.4k` in the classic frame: the session's
/// uncached input and output. It gives way among the first.
fn tokens_field(totals: TokenTotals, ascii: bool) -> Field {
    let (up, down) = if ascii {
        ("in ", " out ")
    } else {
        ("↑", " ↓")
    };
    Field {
        forms: vec![vec![
            Part::new(up, Tone::Dim),
            Part::new(format_tokens(totals.input), Tone::Plain),
            Part::new(down, Tone::Dim),
            Part::new(format_tokens(totals.output), Tone::Plain),
        ]],
        yields: vec![rank::TOTALS],
        shrink: None,
    }
}

/// `cache 99% (R180k W2k)`: the latest prompt's share read from the cache,
/// in [`cache_tone`], then the session's cache reads and writes, each shown
/// once it is not zero, as Pi's footer shows them; the counts give way
/// first and the share at its own rank. Without a share, the counts alone; with neither,
/// nothing.
fn cache_field(cache: CacheReading) -> Option<Field> {
    let label = Part::new(format!("{} ", copy::CACHE), Tone::Dim);
    let counts = [("R", cache.read), ("W", cache.write)]
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(mark, count)| format!("{mark}{}", format_tokens(*count)))
        .collect::<Vec<_>>()
        .join(" ");
    let share = cache
        .hit
        .map(|hit| vec![label.clone(), Part::new(format!("{hit}%"), cache_tone(hit))]);
    match (share, counts.is_empty()) {
        (Some(share), false) => {
            let mut whole = share.clone();
            whole.push(Part::new(format!(" ({counts})"), Tone::Dim));
            Some(Field {
                forms: vec![whole, share],
                yields: vec![rank::TOTALS, rank::CACHE_HIT],
                shrink: None,
            })
        }
        (Some(share), true) => Some(Field {
            forms: vec![share],
            yields: vec![rank::CACHE_HIT],
            shrink: None,
        }),
        (None, false) => Some(Field {
            forms: vec![vec![label, Part::new(counts, Tone::Dim)]],
            yields: vec![rank::TOTALS],
            shrink: None,
        }),
        (None, true) => None,
    }
}

/// `~/bake ⎇ main`: where the session works, read as one place. The
/// directory fills what the other fields leave, cut from its start so the
/// branch at its end stays; at its rank the field steps to the branch alone,
/// which gives way at the branch's rank. Either half may be absent.
fn location_field(input: &StatusInput) -> Option<Field> {
    let branch = input.branch.as_ref().map(|branch| {
        // A detached HEAD wants a look before work is committed on it.
        let name = if branch.detached {
            Part::new(format!("({})", branch.name), Tone::Ink(Token::Notice))
        } else {
            Part::new(branch.name.as_str(), Tone::Plain)
        };
        let glyph = if input.ascii { "on " } else { "⎇ " };
        vec![Part::new(glyph, Tone::Dim), name]
    });
    let fill = |min: usize| {
        Some(Shrink {
            kind: ShrinkKind::Fill,
            rank: rank::CWD,
            min,
        })
    };
    match (input.cwd.is_empty(), branch) {
        (true, None) => None,
        (true, Some(branch)) => Some(Field {
            forms: vec![branch],
            yields: vec![rank::BRANCH],
            shrink: None,
        }),
        (false, None) => Some(Field {
            forms: vec![vec![Part::new(input.cwd.as_str(), Tone::Dim)]],
            yields: Vec::new(),
            shrink: fill(CWD_MIN),
        }),
        (false, Some(branch)) => {
            let mut whole = vec![Part::new(format!("{} ", input.cwd), Tone::Dim)];
            whole.extend(branch.iter().cloned());
            Some(Field {
                forms: vec![whole, branch.clone()],
                yields: vec![rank::CWD, rank::BRANCH],
                // The directory's few cells, and the branch whole.
                shrink: fill(CWD_MIN + 1 + form_width(&branch)),
            })
        }
    }
}

/// Cells the fitted fields take, gaps included.
pub fn fitted_width(fitted: &[Fitted]) -> usize {
    fitted.iter().map(|f| f.width).sum::<usize>() + fitted.len().saturating_sub(1) * FIELD_GAP
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(forms: &[&str], yields: &[u16]) -> Field {
        Field {
            forms: forms
                .iter()
                .map(|text| vec![Part::new(*text, Tone::Plain)])
                .collect(),
            yields: yields.to_vec(),
            shrink: None,
        }
    }

    fn shrinking(text: &str, kind: ShrinkKind, rank: u16, min: usize) -> Field {
        Field {
            forms: vec![vec![Part::new(text, Tone::Plain)]],
            yields: Vec::new(),
            shrink: Some(Shrink { kind, rank, min }),
        }
    }

    fn row(fields: &[Field], room: usize) -> String {
        fit(fields, room)
            .iter()
            .map(|f| drawn(f).iter().map(|p| p.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("  ")
    }

    // The cases below are the TypeScript `fitStatus` tests, case for case.

    #[test]
    fn keeps_every_field_whole_while_they_fit_two_cells_apart() {
        let fields = [field(&["aaaa"], &[]), field(&["bb", "b"], &[20])];
        let widths: Vec<_> = fit(&fields, 8).iter().map(|f| f.width).collect();
        assert_eq!(widths, [4, 2]);
        assert_eq!(row(&fields, 8), "aaaa  bb");
    }

    #[test]
    fn gives_way_rank_by_rank_whatever_the_display_order() {
        let fields = [
            field(&["first-long", "first"], &[30]),
            field(&["second-long", "second"], &[10]),
            field(&["third"], &[20]),
        ];
        assert_eq!(row(&fields, 40), "first-long  second-long  third");
        assert_eq!(row(&fields, 28), "first-long  second  third");
        assert_eq!(row(&fields, 23), "first-long  second");
        assert_eq!(row(&fields, 13), "first  second");
        // Past every rank the row is as narrow as it gets; the renderer clips it.
        assert_eq!(row(&fields, 6), "first  second");
    }

    #[test]
    fn cuts_a_shrinking_field_from_its_end_no_further_than_its_floor() {
        let fields = [
            shrinking("deepseek-v4-flash", ShrinkKind::End, rank::MODEL, MODEL_MIN),
            field(&["ctx ~11%"], &[]),
        ];
        assert_eq!(
            (fit(&fields, 30)[0].width, fit(&fields, 30)[0].cut),
            (17, None)
        );
        assert_eq!(
            (fit(&fields, 22)[0].width, fit(&fields, 22)[0].cut),
            (12, Some(Cut::End))
        );
        assert_eq!(fit(&fields, 18)[0].width, MODEL_MIN);
        // The reading that never yields keeps its cells; the model gives up the rest.
        assert_eq!(
            (fit(&fields, 14)[0].width, fit(&fields, 14)[0].cut),
            (4, Some(Cut::End))
        );
        assert_eq!(row(&fields, 14), "dee…  ctx ~11%");
        assert_eq!(row(&fields, 10), "ctx ~11%");
    }

    #[test]
    fn keeps_a_few_cells_for_the_filler_and_gives_it_what_is_left() {
        let path = shrinking("~/projects/bake", ShrinkKind::Fill, rank::CWD, CWD_MIN);
        let fields = [
            field(&["model"], &[]),
            field(&["in 1k  cache hit 9%", "cache hit 9%"], &[rank::TOTALS]),
            field(&["⎇ main +1", "⎇ main"], &[rank::GIT_COUNTS]),
            path.clone(),
        ];
        assert_eq!(
            row(&fields, 60),
            "model  in 1k  cache hit 9%  ⎇ main +1  ~/projects/bake"
        );
        // Nothing ranked gives way while the filler has its few cells: it is cut instead.
        assert_eq!(
            row(&fields, 50),
            "model  in 1k  cache hit 9%  ⎇ main +1  …jects/bake"
        );
        let last = fit(&fields, 50).pop().unwrap();
        assert_eq!((last.width, last.cut), (11, Some(Cut::Start)));
        // Ranked before the filler, the totals give way to keep them.
        assert_eq!(
            row(&fields, 44),
            "model  cache hit 9%  ⎇ main +1  …ojects/bake"
        );
        // After its rank nothing gives way for it; a tail under its floor is left out.
        assert_eq!(row(&fields, 36), "model  cache hit 9%  ⎇ main +1");
        assert_eq!(row(&fields, 29), "model  cache hit 9%  ⎇ main");
        // Something ranked later giving way frees cells the filler takes back.
        let update = [
            field(&["model"], &[]),
            field(&["update v0.2.0"], &[rank::UPDATE]),
            path,
        ];
        assert_eq!(row(&update, 18), "model  …jects/bake");
    }

    #[test]
    fn measures_in_terminal_cells() {
        assert_eq!(
            form_width(&[
                Part::new("上下文 ", Tone::Plain),
                Part::new("~11%", Tone::Plain)
            ]),
            11
        );
        let fields = [field(&["上下文 ~11%"], &[]), field(&["思考 high"], &[10])];
        assert_eq!(row(&fields, 20), "上下文 ~11%");
    }

    #[test]
    fn a_cut_keeps_each_runs_tone_and_never_splits_a_wide_character() {
        let fitted = Fitted {
            parts: vec![
                Part::new("no ", Tone::Ink(Token::Notice)),
                Part::new("界界界", Tone::Dim),
            ],
            width: 7,
            cut: Some(Cut::End),
        };
        assert_eq!(
            drawn(&fitted),
            [
                Part::new("no ", Tone::Ink(Token::Notice)),
                Part::new("界…", Tone::Dim)
            ]
        );
        let start = Fitted {
            cut: Some(Cut::Start),
            ..fitted
        };
        assert_eq!(drawn(&start), [Part::new("…界界界", Tone::Dim)]);
    }

    #[test]
    fn the_working_directory_reads_from_home() {
        assert_eq!(home_relative("/home/me/bake", Some("/home/me")), "~/bake");
        assert_eq!(home_relative("/home/me", Some("/home/me/")), "~");
        assert_eq!(home_relative("/home/meow", Some("/home/me")), "/home/meow");
        assert_eq!(home_relative("/srv/x", None), "/srv/x");
        assert_eq!(home_relative("/srv/x", Some("")), "/srv/x");
    }

    fn text(form: &[Part]) -> String {
        form.iter().map(|p| p.text.as_str()).collect()
    }

    fn full() -> StatusInput {
        StatusInput {
            model: Some("deepseek-official/deepseek-v4-flash".into()),
            thinking: Some("high".into()),
            context: Some(ContextUsage {
                used: Some(15_200),
                window: 128_000,
            }),
            branch: Some(Branch {
                name: "main".into(),
                detached: false,
            }),
            cwd: "~/bake".into(),
            ascii: false,
            ..StatusInput::default()
        }
    }

    #[test]
    fn reads_as_three_consolidated_fields() {
        let fields = fields(&full());
        let firsts: Vec<_> = fields.iter().map(|f| text(&f.forms[0])).collect();
        assert_eq!(
            firsts,
            [
                "deepseek-v4-flash high",
                "ctx ▰▱▱▱▱ ~11% (15.2k/128k)",
                "~/bake ⎇ main"
            ]
        );
        assert_eq!(fields[0].forms[0][0].tone, Tone::Anchor);
        assert_eq!(fields[0].forms[0][1].tone, Tone::Ink(Token::High));
        assert_eq!(fields[1].forms[1][0].tone, Tone::Dim);
        assert_eq!(fields[1].forms[1][1].tone, Tone::Plain);
        assert_eq!(
            row(&fields, 80),
            "deepseek-v4-flash high  ctx ▰▱▱▱▱ ~11% (15.2k/128k)  ~/bake ⎇ main"
        );
    }

    #[test]
    fn leaves_out_what_the_session_does_not_have() {
        let only = |input: StatusInput| row(&fields(&input), 80);
        assert_eq!(
            only(StatusInput {
                cwd: "~/bake".into(),
                ..StatusInput::default()
            }),
            "no model  ~/bake"
        );
        assert_eq!(
            fields(&StatusInput::default())[0].forms[0][0].tone,
            Tone::Ink(Token::Notice)
        );
        assert_eq!(
            only(StatusInput {
                branch: Some(Branch {
                    name: "1a2b3c4".into(),
                    detached: true,
                }),
                ascii: true,
                ..StatusInput::default()
            }),
            "no model  on (1a2b3c4)"
        );
        let mut no_level = full();
        no_level.thinking = None;
        assert_eq!(fields(&no_level)[0].forms.len(), 1);
    }

    // Bake: the token totals and the cache, Pi's footer readings, come
    // after the context when turned on. The totals and the cache's counts
    // give way first, then the directory, and the hit outlasts the branch's
    // directory.
    #[test]
    fn token_totals_and_the_cache_show_when_on_and_give_way_early() {
        let mut input = full();
        input.tokens = Some(TokenTotals {
            input: 12_000,
            output: 3_400,
        });
        input.cache = Some(CacheReading {
            read: 180_000,
            write: 2_000,
            hit: Some(99),
        });
        let fields = fields(&input);
        assert_eq!(
            row(&fields, 120),
            "deepseek-v4-flash high  ctx ▰▱▱▱▱ ~11% (15.2k/128k)  ↑12k ↓3.4k  cache 99% (R180k W2k)  ~/bake ⎇ main"
        );
        assert_eq!(fields[3].forms[0][0].tone, Tone::Dim);
        assert_eq!(fields[3].forms[0][1].tone, Tone::Ink(Token::Ok));
        assert_eq!(
            row(&fields, 70),
            "deepseek-v4-flash high  ctx ~11%  cache 99%  ~/bake ⎇ main"
        );
        assert_eq!(
            row(&fields, 52),
            "deepseek-v4-flash high  ctx ~11%  cache 99%  ⎇ main"
        );
        assert_eq!(row(&fields, 44), "deepseek-v4-flash high  ctx ~11%  ⎇ main");
        // The classic frame spells the arrows out.
        input.ascii = true;
        assert_eq!(text(&super::fields(&input)[2].forms[0]), "in 12k out 3.4k");
        // Without a measured prompt the cache shows its counts, and before
        // any use, nothing.
        let counts = cache_field(CacheReading {
            read: 0,
            write: 900,
            hit: None,
        })
        .expect("counts");
        assert_eq!(text(&counts.forms[0]), "cache W900");
        assert_eq!(cache_field(CacheReading::default()), None);
        // A provider that reports no writes, or no cache at all, shows what
        // it has.
        let reads = cache_field(CacheReading {
            read: 31_600,
            write: 0,
            hit: Some(89),
        })
        .expect("reads");
        assert_eq!(text(&reads.forms[0]), "cache 89% (R31.6k)");
        let none = cache_field(CacheReading {
            read: 0,
            write: 0,
            hit: Some(0),
        })
        .expect("a share");
        assert_eq!(
            none.forms.iter().map(|form| text(form)).collect::<Vec<_>>(),
            ["cache 0%"]
        );
    }

    #[test]
    fn narrowing_cuts_the_directory_then_drops_the_branch_and_the_level() {
        let mut input = full();
        input.cwd = "~/projects/bake".into();
        let fields = fields(&input);
        assert_eq!(
            row(&fields, 76),
            "deepseek-v4-flash high  ctx ▰▱▱▱▱ ~11% (15.2k/128k)  ~/projects/bake ⎇ main"
        );
        // The directory fills what is left, cut from its start while its
        // few cells fit.
        assert_eq!(
            row(&fields, 70),
            "deepseek-v4-flash high  ctx ▰▱▱▱▱ ~11% (15.2k/128k)  …ects/bake ⎇ main"
        );
        // To keep them, the gauge goes first.
        assert_eq!(
            row(&fields, 64),
            "deepseek-v4-flash high  ctx ~11% (15.2k/128k)  …ects/bake ⎇ main"
        );
        // Then the count, and the directory is cut from its start.
        assert_eq!(
            row(&fields, 50),
            "deepseek-v4-flash high  ctx ~11%  …cts/bake ⎇ main"
        );
        // It keeps six cells of the directory, or steps to the branch alone.
        assert_eq!(
            row(&fields, 47),
            "deepseek-v4-flash high  ctx ~11%  …/bake ⎇ main"
        );
        assert_eq!(row(&fields, 46), "deepseek-v4-flash high  ctx ~11%  ⎇ main");
        // Then the branch goes, and then the level, as in the TypeScript ranks.
        assert_eq!(row(&fields, 37), "deepseek-v4-flash high  ctx ~11%");
        assert_eq!(row(&fields, 28), "deepseek-v4-flash  ctx ~11%");
        assert_eq!(row(&fields, 20), "deepseek-…  ctx ~11%");
        assert_eq!(fitted_width(&fit(&fields, 20)), 20);
    }

    #[test]
    fn a_nearly_full_context_keeps_its_count_longest_and_turns_red() {
        let mut input = full();
        input.context = Some(ContextUsage {
            used: Some(120_000),
            window: 128_000,
        });
        let fields = fields(&input);
        assert_eq!(fields[1].forms[1][1].tone, Tone::Ink(Token::Danger));
        assert_eq!(
            row(&fields, 44),
            "deepseek-v4-flash high  ctx ~93% (120k/128k)"
        );
        assert_eq!(row(&fields, 43), "deepseek-v4-flash  ctx ~93% (120k/128k)");
    }

    // After a compaction the next response has not measured the context:
    // the window stays, the count reads `?`, and it yields as the count does.
    #[test]
    fn an_unknown_context_reads_as_a_question_against_the_window() {
        let mut input = full();
        input.context = Some(ContextUsage {
            used: None,
            window: 128_000,
        });
        let fields = fields(&input);
        assert_eq!(fields[1].forms[0][1].tone, Tone::Plain);
        assert!(row(&fields, 80).contains("  ctx ?/128k  "));
        // The location goes elsewhere, so the count yields at its own rank.
        input.branch = None;
        input.cwd = String::new();
        let fields = super::fields(&input);
        assert_eq!(row(&fields, 34), "deepseek-v4-flash high  ctx ?/128k");
        assert_eq!(row(&fields, 33), "deepseek-v4-flash high  ctx ?");
    }

    #[test]
    fn levels_and_occupancy_warm_as_they_rise() {
        let tones: Vec<_> = ["minimal", "low", "medium", "high", "xhigh", "max", "Turbo"]
            .map(thinking_tone)
            .to_vec();
        assert_eq!(
            tones,
            [
                Tone::Dim,
                Tone::Dim,
                Tone::Plain,
                Tone::Ink(Token::High),
                Tone::Ink(Token::XHigh),
                Tone::Ink(Token::Max),
                Tone::Plain
            ]
        );
        let notice = Tone::Ink(Token::Notice);
        let (warn, danger) = (Tone::Ink(Token::Warn), Tone::Ink(Token::Danger));
        let percents = [10, 59, 60, 70, 79, 80, 89, 90, 100];
        assert_eq!(
            percents.map(context_tone),
            [
                Tone::Plain,
                Tone::Plain,
                notice,
                notice,
                notice,
                warn,
                warn,
                danger,
                danger
            ]
        );
        // The gauge is green while the percentage is still quiet.
        let ok = Tone::Ink(Token::Ok);
        assert_eq!(
            percents.map(gauge_tone),
            [ok, ok, notice, notice, notice, warn, warn, danger, danger]
        );
        // The cache's share warms as it falls.
        assert_eq!(
            [100, 80, 79, 50, 49, 0].map(cache_tone),
            [ok, ok, notice, notice, warn, warn]
        );
    }

    fn line_text(input: &StatusInput, state: SessionState, room: usize) -> String {
        line(input, state, room)
            .iter()
            .map(|p| p.text.as_str())
            .collect()
    }

    // Bake: a five-cell gauge leads the context's reading, filled to the
    // nearest cell and full from 90%, where the context turns `danger`. It
    // gives way first, and an unknown context has none.
    #[test]
    fn the_context_gauge_fills_warms_and_gives_way_first() {
        let cells = [0, 9, 10, 23, 30, 50, 69, 70, 89, 90, 100].map(gauge_filled);
        assert_eq!(cells, [0, 0, 1, 1, 2, 3, 3, 4, 4, 5, 5]);
        let context = |used: u64, ascii: bool| {
            context_field(
                ContextUsage {
                    used: Some(used),
                    window: 100_000,
                },
                ascii,
            )
        };
        let gauged = context(23_000, false);
        assert_eq!(text(&gauged.forms[0]), "ctx ▰▱▱▱▱ ~23% (23k/100k)");
        assert_eq!(form_width(&gauged.forms[0][1..3]), GAUGE_CELLS);
        assert_eq!(gauged.yields, [rank::CONTEXT_GAUGE, rank::CONTEXT_ABSOLUTE]);
        assert_eq!(
            gauged.forms[0][1..3],
            [
                Part::new("▰", Tone::Ink(Token::Ok)),
                Part::new("▱▱▱▱", Tone::Dim)
            ]
        );
        assert_eq!(gauged.forms[0][3].tone, Tone::Plain);
        let hot = context(85_000, false);
        assert_eq!(
            hot.forms[0][1..],
            [
                Part::new("▰▰▰▰", Tone::Ink(Token::Warn)),
                Part::new("▱", Tone::Dim),
                Part::new(" ~85% (85k/100k)", Tone::Ink(Token::Warn)),
            ]
        );
        assert_eq!(
            hot.yields,
            [rank::CONTEXT_GAUGE, rank::CONTEXT_ABSOLUTE_WARM]
        );
        let full = context(95_000, false);
        assert_eq!(
            full.forms[0][1],
            Part::new("▰▰▰▰▰", Tone::Ink(Token::Danger))
        );
        assert_eq!(full.forms[0].len(), 3, "no empty cells");
        // The classic frame draws it in ASCII; an empty one has no fill.
        assert_eq!(
            text(&context(64_000, true).forms[0]),
            "ctx [###--] ~64% (64k/100k)"
        );
        assert_eq!(
            context(64_000, true).forms[0][2].tone,
            Tone::Ink(Token::Notice)
        );
        assert_eq!(text(&context(0, true).forms[0]), "ctx [-----] ~0% (0/100k)");
        let unknown = context_field(
            ContextUsage {
                used: None,
                window: 100_000,
            },
            false,
        );
        assert!(unknown.forms.iter().all(|form| !text(form).contains('▱')));
    }

    // Bake: the state dot leads the row and never gives way; each state has
    // its own glyph, so it reads without colour.
    #[test]
    fn the_state_dot_leads_the_row_in_its_own_glyph_and_never_gives_way() {
        let states = [
            SessionState::Idle,
            SessionState::Working,
            SessionState::Waiting,
            SessionState::Failed,
        ];
        assert_eq!(
            states.map(|state| dot(state, false)),
            [
                Part::new("○", Tone::Dim),
                Part::new("●", Tone::Ink(Token::Accent)),
                Part::new("◆", Tone::Ink(Token::Notice)),
                Part::new("!", Tone::Ink(Token::Danger)),
            ]
        );
        assert_eq!(
            states.map(|state| dot(state, true).text),
            ["o", "*", "?", "!"]
        );
        for ascii in [false, true] {
            let glyphs: std::collections::HashSet<_> = states
                .map(|state| dot(state, ascii).text)
                .into_iter()
                .collect();
            assert_eq!(glyphs.len(), states.len());
            assert!(
                states
                    .iter()
                    .all(|&state| dot(state, ascii).text.width() == 1)
            );
        }
        let input = full();
        assert_eq!(
            line_text(&input, SessionState::Working, 80),
            "● deepseek-v4-flash high  ctx ▰▱▱▱▱ ~11% (15.2k/128k)  ~/bake ⎇ main"
        );
        // The fields fit the room the dot leaves.
        assert_eq!(
            line_text(&input, SessionState::Idle, 40),
            format!("○ {}", row(&fields(&input), 40 - DOT_WIDTH))
        );
        for room in 0..90 {
            let drawn = line_text(&input, SessionState::Failed, room);
            assert!(drawn.width() <= room.max(1), "{room}: {drawn}");
            assert_eq!(drawn.starts_with('!'), room > 0, "{room}");
        }
        assert_eq!(line_text(&input, SessionState::Failed, 9), "!");
        assert_eq!(line_text(&input, SessionState::Failed, 12), "! ctx ~11%");
        assert_eq!(
            line_text(&input, SessionState::Failed, 22),
            "! deepseek-…  ctx ~11%"
        );
        let mut classic = input.clone();
        classic.ascii = true;
        assert!(line_text(&classic, SessionState::Waiting, 80).starts_with("? deepseek"));
    }

    #[test]
    fn a_detached_branch_wants_a_look() {
        let mut input = full();
        let branch_tone = |input: &StatusInput| {
            let fields = fields(input);
            let location = fields.last().unwrap().forms[1].clone();
            location[1].tone
        };
        assert_eq!(branch_tone(&input), Tone::Plain);
        input.branch = Some(Branch {
            name: "1a2b3c4".into(),
            detached: true,
        });
        assert_eq!(branch_tone(&input), Tone::Ink(Token::Notice));
    }

    #[test]
    fn token_counts_abbreviate_as_the_typescript_status_line_does() {
        let cases = [
            (950, "950"),
            (1_000, "1k"),
            (15_200, "15.2k"),
            (999_999, "1M"),
            (1_260_000, "1.3M"),
        ];
        for (tokens, text) in cases {
            assert_eq!(format_tokens(tokens), text, "{tokens}");
        }
        assert_eq!(
            context_percent(ContextUsage {
                used: Some(127_999),
                window: 128_000
            }),
            99
        );
        assert_eq!(
            context_percent(ContextUsage {
                used: Some(5),
                window: 0
            }),
            0
        );
        assert_eq!(compact_model("a/b/model-x"), "model-x");
    }
}
