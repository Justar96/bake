//! The status line's fields and the order they give way in as the row
//! narrows; a port of the TypeScript `status-line.ts` fitting.
//!
//! Fields keep their order. Each gives way at its own rank, whole or to a
//! shorter complete reading, never cut mid-word, except the two fields that
//! shrink: the model, cut from its end, and the working directory, which
//! fills what the others leave and is cut from its start.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::copy;

/// The order fields give way in, first to yield first, in tenths so the
/// TypeScript table's half ranks stay exact. Only some fields exist in the
/// preview; the rest keep their places for the fields that will.
pub mod rank {
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

/// How a run of a field is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// The terminal's own foreground.
    #[default]
    Plain,
    /// Supporting text.
    Dim,
    /// Something waiting on the user, such as a missing model.
    Waiting,
}

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
pub fn fit(fields: &[Field], room: usize) -> Vec<Fitted> {
    let mut form = vec![0usize; fields.len()];
    let mut shrunk = vec![false; fields.len()];
    let shown = |form: &[usize], i: usize| form[i] < fields[i].forms.len();
    let full = |form: &[usize], i: usize| form_width(&fields[i].forms[form[i]]);
    let filler = |i: usize| matches!(fields[i].shrink, Some(s) if s.kind == ShrinkKind::Fill);
    // Cells a field counts for while ranks apply: the filler its minimum
    // until its rank and nothing after it, a field cut from its end its
    // minimum after its rank, and every other field its reading.
    let demand = |form: &[usize], shrunk: &[bool], i: usize| match fields[i].shrink {
        None => full(form, i),
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
    let placed = |i: usize| shown(&form, i) && !filler(i) && demand(&form, &shrunk, i) > 0;
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
        if filler(i) {
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

/// The preview's status fields, in display order: no model, which says the
/// preview is not connected until the model's rank; the quit key, which the
/// preview names because it has no other help; and the working directory.
pub fn preview_fields(cwd: &str) -> Vec<Field> {
    let no_model = Part::new(copy::NO_MODEL_FIELD, Tone::Waiting);
    let mut fields = vec![
        Field {
            forms: vec![
                vec![
                    no_model.clone(),
                    Part::new(format!("  {}", copy::NO_MODEL_HINT), Tone::Dim),
                ],
                vec![no_model],
            ],
            yields: vec![rank::MODEL],
            shrink: None,
        },
        Field {
            forms: vec![vec![Part::new(copy::QUIT_KEY, Tone::Dim)]],
            yields: vec![rank::UPDATE],
            shrink: None,
        },
    ];
    if !cwd.is_empty() {
        fields.push(Field {
            forms: vec![vec![Part::new(cwd, Tone::Dim)]],
            yields: Vec::new(),
            shrink: Some(Shrink {
                kind: ShrinkKind::Fill,
                rank: rank::CWD,
                min: CWD_MIN,
            }),
        });
    }
    fields
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
                Part::new("no ", Tone::Waiting),
                Part::new("界界界", Tone::Dim),
            ],
            width: 7,
            cut: Some(Cut::End),
        };
        assert_eq!(
            drawn(&fitted),
            [Part::new("no ", Tone::Waiting), Part::new("界…", Tone::Dim)]
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

    #[test]
    fn the_preview_status_gives_up_the_directory_then_the_key_then_the_hint() {
        let fields = preview_fields("~/projects/bake");
        assert_eq!(
            row(&fields, 80),
            "no model  rust preview  Ctrl+C quits  ~/projects/bake"
        );
        assert_eq!(
            row(&fields, 46),
            "no model  rust preview  Ctrl+C quits  …ts/bake"
        );
        // Past the filler's rank its tail is too short to name anything.
        assert_eq!(row(&fields, 40), "no model  rust preview  Ctrl+C quits");
        // The key giving way frees cells the directory takes back.
        assert_eq!(row(&fields, 30), "no model  rust preview  …/bake");
        assert_eq!(row(&fields, 12), "no model");
    }
}
