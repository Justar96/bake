//! Colour for a shell call's output, as the command would have drawn it in a
//! terminal.
//!
//! Bake runs commands with `NO_COLOR=1` and `TERM=dumb`, so their output
//! reaches the transcript plain; that keeps colour codes out of what the
//! model reads. The preview restores the look the user knows by recognising
//! the shape of familiar output: a search's `path:line:` prefix, a test
//! runner's pass and fail markers, `git status --short`, and `error:` and
//! `warning:` lines from any command. Output that does carry SGR colour, from
//! a command that forces it, is drawn with that colour instead.
//!
//! Every escape sequence is removed from the text, SGR or not, and so is
//! every other control character; a tab becomes four spaces, so no output can move the cursor
//! or change the terminal. Styling never changes a byte of the text left, so
//! it cannot change a wrap.

use std::ops::Range;

use ratatui_core::style::{Color, Modifier, Style};

use crate::activity::Tones;

/// The family of command whose output shape is recognised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// `rg`, `grep`, `ag`, `ack`, `git grep`: lines open with `path:line:`.
    Search,
    /// A test runner: pass and fail markers and their counts.
    Test,
    /// `git status --short`: a two-letter code before each path.
    GitStatus,
    Other,
}

/// The family of the program `command` runs first, past `NAME=value`
/// assignments and launchers such as `env`, `npx`, and `sudo`.
pub fn family(command: &str) -> Family {
    let words: Vec<&str> = command
        .split_whitespace()
        .skip_while(|word| {
            word.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            }) || matches!(
                *word,
                "env" | "sudo" | "npx" | "bunx" | "pnpx" | "time" | "command"
            )
        })
        .collect();
    let program = words
        .first()
        .map_or("", |w| w.rsplit('/').next().unwrap_or(w));
    let sub = words.get(1).copied().unwrap_or("");
    let after_run = if sub == "run" {
        words.get(2).copied().unwrap_or("")
    } else {
        sub
    };
    match program {
        "rg" | "grep" | "egrep" | "fgrep" | "ag" | "ack" => Family::Search,
        "git" if sub == "grep" => Family::Search,
        "git"
            if sub == "status"
                && words
                    .iter()
                    .any(|w| matches!(*w, "-s" | "--short" | "--porcelain")) =>
        {
            Family::GitStatus
        }
        "vitest" | "jest" | "pytest" | "mocha" | "nextest" => Family::Test,
        "bun" | "npm" | "pnpm" | "yarn" | "deno"
            if after_run == "test" || after_run.starts_with("test:") =>
        {
            Family::Test
        }
        "cargo" | "go" if sub == "test" || sub == "nextest" => Family::Test,
        "python" | "python3" if words.get(2) == Some(&"pytest") => Family::Test,
        _ => Family::Other,
    }
}

/// What a run of output means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Path,
    LineNumber,
    Pass,
    Fail,
    Skip,
    Error,
    Warning,
    /// Timings and other asides, such as `[1.20ms]`.
    Aside,
    /// A git status code: changed, staged, removed, or untracked.
    Changed,
    Staged,
    Removed,
    Untracked,
}

/// A mark's style. The colours are the ones the commands use: ripgrep's
/// magenta path and green line number, a runner's green and red, git's red
/// unstaged and green staged codes.
pub fn style(mark: Mark, tones: Tones) -> Style {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let (rgb, ansi, strong) = match mark {
        Mark::Aside => return Style::new().add_modifier(Modifier::DIM),
        Mark::Path => ((0xf0, 0xab, 0xfc), Color::Magenta, false),
        Mark::LineNumber | Mark::Pass | Mark::Staged => ((0x86, 0xef, 0xac), Color::Green, false),
        Mark::Fail | Mark::Removed => ((0xf8, 0x71, 0x71), Color::Red, true),
        Mark::Error => ((0xf8, 0x71, 0x71), Color::Red, true),
        Mark::Changed | Mark::Untracked => ((0xf8, 0x71, 0x71), Color::Red, false),
        Mark::Skip | Mark::Warning => (
            (0xfd, 0xe6, 0x8a),
            Color::Yellow,
            matches!(mark, Mark::Warning),
        ),
    };
    let base = if strong { bold } else { Style::new() };
    match tones {
        Tones::TrueColor => base.fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        Tones::Ansi => base.fg(ansi),
        // Without colour, what failed stays bold.
        Tones::None => base,
    }
}

/// One line of output, cleaned, with the styles of its runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Styled {
    pub text: String,
    pub runs: Vec<(Range<usize>, Style)>,
}

/// Cleans `line` and styles it: by its own SGR colour when it carries any,
/// otherwise by the shape `family` recognises.
pub fn styled(line: &str, family: Family, tones: Tones) -> Styled {
    let (text, sgr) = parse_sgr(line, tones);
    if sgr.iter().any(|(_, style)| *style != Style::new()) {
        return Styled { text, runs: sgr };
    }
    let runs = marks(&text, family)
        .into_iter()
        .map(|(range, mark)| (range, style(mark, tones)))
        .collect();
    Styled { text, runs }
}

/// The marks of a plain line, as byte ranges in order.
pub fn marks(line: &str, family: Family) -> Vec<(Range<usize>, Mark)> {
    let lower = line.trim_start().to_ascii_lowercase();
    let indent = line.len() - line.trim_start().len();
    // Any command: a diagnostic's label.
    for (label, mark) in [("error", Mark::Error), ("warning", Mark::Warning)] {
        if let Some(rest) = lower.strip_prefix(label) {
            let end = if rest.starts_with(':') {
                label.len() + 1
            } else if rest.starts_with('[') {
                rest.find("]:").map_or(0, |i| label.len() + i + 2)
            } else {
                0
            };
            if end > 0 {
                return vec![(indent..indent + end, mark)];
            }
        }
    }
    match family {
        Family::Search => search(line),
        Family::Test => test(line),
        Family::GitStatus => git_status(line),
        Family::Other => Vec::new(),
    }
}

/// `path:line:` or `path:line:column:` at the start of a line.
fn search(line: &str) -> Vec<(Range<usize>, Mark)> {
    let mut out = Vec::new();
    let Some(colon) = line.find(':').filter(|&i| i > 0) else {
        return out;
    };
    let mut at = colon + 1;
    let mut numbers = Vec::new();
    while let Some(len) = line[at..].find(':').filter(|&len| len > 0) {
        if !line[at..at + len].bytes().all(|b| b.is_ascii_digit()) || numbers.len() == 2 {
            break;
        }
        numbers.push(at..at + len);
        at += len + 1;
    }
    if numbers.is_empty() {
        return out;
    }
    out.push((0..colon, Mark::Path));
    out.extend(numbers.into_iter().map(|range| (range, Mark::LineNumber)));
    out
}

/// A test runner's markers and counts.
fn test(line: &str) -> Vec<(Range<usize>, Mark)> {
    let mut out = Vec::new();
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    // A marker opening the line: `(pass)`, `✓`, `PASS`, `ok`, and the rest.
    const OPENERS: &[(&str, Mark)] = &[
        ("(pass)", Mark::Pass),
        ("(fail)", Mark::Fail),
        ("(skip)", Mark::Skip),
        ("(todo)", Mark::Skip),
        ("✓", Mark::Pass),
        ("✔", Mark::Pass),
        ("✗", Mark::Fail),
        ("✘", Mark::Fail),
        ("×", Mark::Fail),
        ("↓", Mark::Skip),
        ("PASS", Mark::Pass),
        ("FAIL", Mark::Fail),
        ("ok", Mark::Pass),
        ("--- PASS", Mark::Pass),
        ("--- FAIL", Mark::Fail),
    ];
    for (marker, mark) in OPENERS {
        let after = trimmed.get(marker.len()..);
        if trimmed.starts_with(marker)
            && after.is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        {
            out.push((indent..indent + marker.len(), *mark));
            break;
        }
    }
    // `test name ... ok` and `... FAILED` at the end, as `cargo test` prints.
    for (suffix, mark) in [
        (" ... ok", Mark::Pass),
        (" ... FAILED", Mark::Fail),
        (" ... ignored", Mark::Skip),
    ] {
        if let Some(head) = line.strip_suffix(suffix) {
            out.push((head.len() + 5..line.len(), mark));
        }
    }
    // `test result: ok.` and `test result: FAILED.`
    if let Some(at) = line.find("test result: ") {
        let word = at + "test result: ".len();
        for (status, mark) in [("ok", Mark::Pass), ("FAILED", Mark::Fail)] {
            if line[word..].starts_with(status) {
                out.push((word..word + status.len(), mark));
            }
        }
    }
    // Counts: ` 3 pass`, `1 fail`, `2 passed`, `0 failed`; a zero recedes.
    let words: Vec<(usize, &str)> = line
        .split(' ')
        .scan(0, |at, word| {
            let start = *at;
            *at += word.len() + 1;
            Some((start, word))
        })
        .filter(|(_, word)| !word.is_empty())
        .collect();
    for pair in words.windows(2) {
        let ((n_at, number), (w_at, word)) = (pair[0], pair[1]);
        let word = word.trim_end_matches([',', ';', '.']);
        let Ok(count) = number.parse::<u64>() else {
            continue;
        };
        let mark = match word {
            "pass" | "passed" | "passing" => Mark::Pass,
            "fail" | "failed" | "failing" | "failures" => Mark::Fail,
            "skip" | "skipped" | "pending" | "todo" | "ignored" => Mark::Skip,
            _ => continue,
        };
        let mark = if count == 0 { Mark::Aside } else { mark };
        out.push((n_at..w_at + word.len(), mark));
    }
    // A bracketed timing, `[1.20ms]`, at the end.
    if let Some(open) = line.rfind(" [")
        && line.ends_with("s]")
        && line[open + 2..line.len() - 1]
            .trim_end_matches(['m', 'µ', 'u', 's'])
            .parse::<f64>()
            .is_ok()
    {
        out.push((open + 1..line.len(), Mark::Aside));
    }
    out.sort_by_key(|(range, _)| range.start);
    // Drop any run that overlaps the one before it.
    let mut end = 0;
    out.retain(|(range, _)| {
        let keep = range.start >= end;
        if keep {
            end = range.end;
        }
        keep
    });
    out
}

/// `XY path`: X the staged change, Y the unstaged one, `??` untracked.
fn git_status(line: &str) -> Vec<(Range<usize>, Mark)> {
    let bytes = line.as_bytes();
    if bytes.len() < 4 || bytes[2] != b' ' {
        return Vec::new();
    }
    if &bytes[..2] == b"??" {
        return vec![(0..2, Mark::Untracked)];
    }
    let code = |b: u8| matches!(b, b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'T' | b' ');
    if !code(bytes[0]) || !code(bytes[1]) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if bytes[0] != b' ' {
        out.push((0..1, Mark::Staged));
    }
    if bytes[1] != b' ' {
        out.push((
            1..2,
            if bytes[1] == b'D' {
                Mark::Removed
            } else {
                Mark::Changed
            },
        ));
    }
    out
}

/// The text of `line` without escape sequences or control characters, tabs
/// as four spaces, and the style its SGR sequences gave each run.
pub fn parse_sgr(line: &str, tones: Tones) -> (String, Vec<(Range<usize>, Style)>) {
    let mut text = String::with_capacity(line.len());
    let mut runs: Vec<(Range<usize>, Style)> = Vec::new();
    let mut style = Style::new();
    let mut chars = line.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek().map(|&(_, c)| c) {
                // CSI: parameters, then a final byte from `@` to `~`.
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut last = None;
                    for (_, p) in chars.by_ref() {
                        if ('@'..='~').contains(&p) {
                            last = Some(p);
                            break;
                        }
                        params.push(p);
                    }
                    if last == Some('m') {
                        style = apply_sgr(style, &params, tones);
                    }
                }
                // OSC, such as a hyperlink: up to BEL or ST; its text stays.
                Some(']') => {
                    chars.next();
                    while let Some((_, p)) = chars.next() {
                        if p == '\u{7}' {
                            break;
                        }
                        if p == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                // Any other escape is two characters.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if c.is_control() && c != '\t' {
            continue;
        }
        let at = text.len();
        // A tab is four spaces, as the TypeScript `toolText` draws it.
        if c == '\t' {
            text.push_str("    ");
        } else {
            text.push(c);
        }
        match runs.last_mut() {
            Some((range, last)) if *last == style && range.end == at => range.end = text.len(),
            _ => runs.push((at..text.len(), style)),
        }
    }
    runs.retain(|(_, style)| *style != Style::new());
    (text, runs)
}

/// `style` after one SGR sequence's parameters, with colours made to suit
/// `tones`: kept on a truecolor terminal, brought to the nearest of sixteen
/// on an ANSI one, and dropped under `NO_COLOR`.
fn apply_sgr(mut style: Style, params: &str, tones: Tones) -> Style {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split([';', ':'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let colour = |c: Color| match tones {
        Tones::TrueColor => Some(c),
        Tones::Ansi => Some(sixteen(c)),
        Tones::None => None,
    };
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        let extended = |i: &mut usize| -> Option<Color> {
            match codes.get(*i + 1) {
                Some(5) => {
                    *i += 2;
                    codes.get(*i).map(|&n| Color::Indexed(n.min(255) as u8))
                }
                Some(2) => {
                    *i += 4;
                    let rgb = codes.get(*i - 2..=*i)?;
                    Some(Color::Rgb(
                        rgb[0].min(255) as u8,
                        rgb[1].min(255) as u8,
                        rgb[2].min(255) as u8,
                    ))
                }
                _ => None,
            }
        };
        match code {
            0 => style = Style::new(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style.fg = colour(Color::Indexed((code - 30) as u8)),
            90..=97 => style.fg = colour(Color::Indexed((code - 90 + 8) as u8)),
            40..=47 => style.bg = colour(Color::Indexed((code - 40) as u8)),
            100..=107 => style.bg = colour(Color::Indexed((code - 100 + 8) as u8)),
            38 => style.fg = extended(&mut i).and_then(colour),
            48 => style.bg = extended(&mut i).and_then(colour),
            39 => style.fg = None,
            49 => style.bg = None,
            _ => {}
        }
        i += 1;
    }
    // The style is the whole state, never a patch, so nothing is removed.
    style.sub_modifier = Modifier::empty();
    style
}

/// The nearest of the sixteen ANSI colours, for a terminal without more.
fn sixteen(colour: Color) -> Color {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    let rgb = match colour {
        Color::Indexed(n) if n < 16 => return Color::Indexed(n),
        Color::Indexed(n) if n >= 232 => {
            let v = 8 + 10 * (n - 232);
            (v, v, v)
        }
        Color::Indexed(n) => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        Color::Rgb(r, g, b) => (r, g, b),
        other => return other,
    };
    let distance = |(r, g, b): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(r, rgb.0) + d(g, rgb.1) + d(b, rgb.2)
    };
    let nearest = (0..16).min_by_key(|&i| distance(BASE[i])).unwrap_or(7);
    Color::Indexed(nearest as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(line: &str, family: Family) -> Vec<(&str, Mark)> {
        marks(line, family)
            .into_iter()
            .map(|(range, mark)| (&line[range], mark))
            .collect()
    }

    #[test]
    fn the_program_names_the_family() {
        assert_eq!(family(r#"rg -n "x" -g '*.ts'"#), Family::Search);
        assert_eq!(family("git grep -n TODO"), Family::Search);
        assert_eq!(family("bun test tests/parser.test.ts"), Family::Test);
        assert_eq!(family("CI=1 npx vitest run"), Family::Test);
        assert_eq!(family("pnpm run test:unit"), Family::Test);
        assert_eq!(family("cargo test --workspace"), Family::Test);
        assert_eq!(family("/usr/bin/grep -rn x ."), Family::Search);
        assert_eq!(family("git status --short"), Family::GitStatus);
        assert_eq!(family("git status"), Family::Other);
        assert_eq!(family("bun run build"), Family::Other);
        assert_eq!(family(""), Family::Other);
    }

    #[test]
    fn a_search_line_colours_its_path_and_numbers() {
        use Mark::*;
        assert_eq!(
            shape(
                "packages/app/src/controller.ts:45:  ctx.effect(x)",
                Family::Search
            ),
            [("packages/app/src/controller.ts", Path), ("45", LineNumber)]
        );
        assert_eq!(
            shape("src/a.rs:12:7:fn x()", Family::Search),
            [("src/a.rs", Path), ("12", LineNumber), ("7", LineNumber)]
        );
        // A colon that is not a location leaves the line plain.
        assert!(shape("note: see above", Family::Search).is_empty());
        assert!(shape("a:b:c", Family::Search).is_empty());
    }

    #[test]
    fn a_test_run_colours_its_markers_counts_and_timings() {
        use Mark::*;
        assert_eq!(
            shape("(pass) splits fields [0.12ms]", Family::Test),
            [("(pass)", Pass), ("[0.12ms]", Aside)]
        );
        assert_eq!(
            shape("(fail) rejects an unterminated quote", Family::Test),
            [("(fail)", Fail)]
        );
        assert_eq!(shape(" 3 pass", Family::Test), [("3 pass", Pass)]);
        assert_eq!(shape(" 0 fail", Family::Test), [("0 fail", Aside)]);
        assert_eq!(
            shape("test parser::quotes ... ok", Family::Test),
            [("ok", Pass)]
        );
        assert_eq!(
            shape("test parser::open ... FAILED", Family::Test),
            [("FAILED", Fail)]
        );
        assert_eq!(
            shape(
                "test result: FAILED. 3 passed; 1 failed; 0 ignored",
                Family::Test
            ),
            [
                ("FAILED", Fail),
                ("3 passed", Pass),
                ("1 failed", Fail),
                ("0 ignored", Aside)
            ]
        );
        assert_eq!(
            shape("  ✓ src/a.test.ts (4 tests) 12ms", Family::Test),
            [("✓", Pass)]
        );
        // A word that only starts like a marker is not one.
        assert!(shape("okay then", Family::Test).is_empty());
    }

    #[test]
    fn git_status_and_diagnostics_from_any_command() {
        use Mark::*;
        assert_eq!(shape(" M src/a.ts", Family::GitStatus), [("M", Changed)]);
        assert_eq!(shape("M  src/a.ts", Family::GitStatus), [("M", Staged)]);
        assert_eq!(shape("?? new.ts", Family::GitStatus), [("??", Untracked)]);
        assert!(shape("hello", Family::GitStatus).is_empty());
        assert_eq!(
            shape("error[E0425]: cannot find value", Family::Other),
            [("error[E0425]:", Error)]
        );
        assert_eq!(
            shape("  warning: unused import", Family::Other),
            [("warning:", Warning)]
        );
        assert!(shape("errors were found", Family::Other).is_empty());
    }

    #[test]
    fn sgr_colour_is_kept_and_every_other_escape_removed() {
        let line = "\u{1b}[32m(pass)\u{1b}[0m ok \u{1b}[1;38;2;255;0;0mred\u{1b}[22;39m \u{1b}[2K\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}\r";
        let (text, runs) = parse_sgr(line, Tones::TrueColor);
        assert_eq!(text, "(pass) ok red link");
        let spans: Vec<(&str, Style)> = runs.iter().map(|(r, s)| (&text[r.clone()], *s)).collect();
        assert_eq!(
            spans,
            [
                ("(pass)", Style::new().fg(Color::Indexed(2))),
                (
                    "red",
                    Style::new()
                        .fg(Color::Rgb(255, 0, 0))
                        .add_modifier(Modifier::BOLD)
                ),
            ]
        );
        // Sixteen colours bring a true colour to its nearest; no colour drops it.
        let (_, ansi) = parse_sgr("\u{1b}[38;2;250;70;70mx", Tones::Ansi);
        assert_eq!(ansi[0].1.fg, Some(Color::Indexed(9)));
        let (_, none) = parse_sgr("\u{1b}[1;31mx", Tones::None);
        assert_eq!(none[0].1, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(sixteen(Color::Indexed(196)), Color::Indexed(1));
        // A line with its own colour is not styled by shape as well.
        let styled = styled(
            "\u{1b}[35msrc/a.ts\u{1b}[0m:1:x",
            Family::Search,
            Tones::TrueColor,
        );
        assert_eq!(styled.runs.len(), 1);
    }

    #[test]
    fn cleaning_never_leaves_a_control_character() {
        for line in [
            "\u{1b}",
            "\u{1b}[",
            "\u{1b}[31",
            "a\u{0}b\u{7}c\u{1b}cd",
            "\u{1b}]unterminated",
            "tab\there",
        ] {
            let (text, runs) = parse_sgr(line, Tones::TrueColor);
            assert!(
                !text.chars().any(|c| c.is_control() && c != '\t'),
                "{line:?} -> {text:?}"
            );
            assert!(runs.iter().all(|(r, _)| r.end <= text.len()));
        }
    }
}
