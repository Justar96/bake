//! The bodies of the call cards whose output is not a plain preview: a
//! search's matches grouped under their files, a listing's first entries,
//! and a web search's results by title and host; and the readers of the
//! output shapes behind them, which the projection also counts with.
//!
//! Bake's own. Pi's renderers for these tools, as checked at Pi v0.78.0
//! (`formatGrepResult`, `formatFindResult`, `formatLsResult`), print the
//! result's raw lines up to a count. Here each body reads its tool's lines
//! in the shape the tool writes them ([`crate::transcript::Kind`] says which
//! shape a card's output has), and draws any line it cannot read as plain
//! output, so a shape it does not expect still loses nothing. What a body
//! leaves out it counts.
//!
//! Output is untrusted: every line is cleaned of escape sequences and
//! control characters before it is read, and no input panics.

use std::collections::HashSet;
use std::ops::Range;

use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};

use crate::activity::Tones;
use crate::copy;
use crate::shell_output;
use crate::transcript::{
    Detail, EXPANDED_ROWS, Look, RESULT_LINE_ROWS, RESULT_LINES, bounded, clip_rows, dim,
    hang_runs, note, output_tone, pad,
};

/// Matches a search's card draws before it counts the rest.
pub const CARD_MATCHES: usize = RESULT_LINES;
/// Lines, context lines included, a search's card draws at most, so a
/// search with wide context stays a few rows.
pub const CARD_MATCH_LINES: usize = 2 * RESULT_LINES;

/// One line of the `grep` tool's output: `path:12: text` for a match and
/// `path-12- text` for a context line around one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrepLine<'a> {
    pub path: &'a str,
    /// The line number's digits.
    pub number: &'a str,
    /// The line's text, after the space that follows the number.
    pub text: &'a str,
    /// A match, not a context line.
    pub matched: bool,
}

/// Reads `line` as the `grep` tool writes it (`GrepFormatter` in
/// `bake-coding-agent`), or `None` when it is not in that shape. The path
/// ends at the first `:12:` or `-12-` that a space or the line's end
/// follows, so a path holding such a run is split there; the text may hold
/// anything.
pub fn grep_line(line: &str) -> Option<GrepLine<'_>> {
    let bytes = line.as_bytes();
    // Both separators are ASCII, which never occurs inside a multi-byte
    // character, so every index below is a character boundary.
    for (at, &separator) in bytes.iter().enumerate().skip(1) {
        if separator != b':' && separator != b'-' {
            continue;
        }
        let digits = bytes[at + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let close = at + 1 + digits;
        if digits == 0 || bytes.get(close) != Some(&separator) {
            continue;
        }
        let text = match bytes.get(close + 1) {
            // A match on an empty last line loses its space to the trim of
            // the result's end.
            None => "",
            Some(b' ') => &line[close + 2..],
            Some(_) => continue,
        };
        return Some(GrepLine {
            path: &line[..at],
            number: &line[at + 1..close],
            text,
            matched: separator == b':',
        });
    }
    None
}

/// The matches among `lines` and the files they are in, or `None` when no
/// line is a match, so the lines are not a search's output.
pub fn grep_counts(lines: &[String]) -> Option<(usize, usize)> {
    let mut files = HashSet::new();
    let mut matches = 0;
    for line in lines {
        if let Some(found) = grep_line(line).filter(|found| found.matched) {
            matches += 1;
            files.insert(found.path);
        }
    }
    (matches > 0).then_some((matches, files.len()))
}

/// The host a web address names, without its `www.`: `docs.rs` for
/// `https://docs.rs/ratatui`; `None` for text that is not an `http` or
/// `https` address with a host.
pub fn host(url: &str) -> Option<&str> {
    let scheme = url.find("://")?;
    if !["http", "https"]
        .iter()
        .any(|name| url[..scheme].eq_ignore_ascii_case(name))
    {
        return None;
    }
    let rest = &url[scheme + 3..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty() && !host.contains(char::is_whitespace)).then_some(host)
}

/// A byte count as the jobs panel words one: `512 B`, `12.4 KB`, `3.1 MB`.
pub fn size(bytes: usize) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1_000.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
    }
}

/// `count` with its noun from a `(one, many)` pair: `1 match`, `5 matches`.
pub fn count(count: usize, (one, many): (&str, &str)) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// A count of what a body left out: `3 more matches`.
fn left_out(left: usize, (one, many): (&str, &str)) -> String {
    format!(
        "{left} {} {}",
        copy::MORE,
        if left == 1 { one } else { many }
    )
}

/// How many of `total` items a card draws when it shows `limit`: all of
/// them when only one would be left out, since a count standing for one
/// item is that item, as a preview's is.
pub(crate) fn shown(total: usize, limit: usize) -> usize {
    if total <= limit + 1 { total } else { limit }
}

/// `line` with no escape sequence or control character, and tabs as
/// spaces.
fn clean(line: &str) -> String {
    shell_output::parse_sgr(line, Tones::None).0
}

/// The nonblank lines of `output`, cleaned.
fn cleaned(output: &[String]) -> Vec<String> {
    output
        .iter()
        .map(|line| clean(line))
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// The run that dims `path`'s directory, as a call's head dims a path's:
/// everything to its last `/`, but a directory's own trailing `/` stays
/// with its name.
fn path_runs(path: &str) -> Vec<(Range<usize>, Style)> {
    let name_end = path.strip_suffix('/').unwrap_or(path);
    match name_end.rfind('/') {
        Some(at) => vec![(0..at + 1, dim())],
        None => Vec::new(),
    }
}

/// The cells `lead` takes.
fn cells(lead: &[Span<'static>]) -> usize {
    lead.iter().map(Span::width).sum()
}

/// `text` wrapped under `lead`, each row opening with it; in a preview, cut
/// with `…` past [`RESULT_LINE_ROWS`] rows, as a preview's line is.
fn row(
    text: &str,
    runs: &[(Range<usize>, Style)],
    base: Style,
    width: usize,
    (first, rest): (Vec<Span<'static>>, Vec<Span<'static>>),
    detail: Detail,
) -> Vec<Line<'static>> {
    let indent = cells(&first);
    let mut rows = hang_runs(text, runs, base, width, indent, first, rest);
    if detail == Detail::Preview {
        clip_rows(&mut rows, RESULT_LINE_ROWS);
    }
    rows
}

/// The rows counting what a body left out, such as `⋯ 3 more matches`.
fn more(lead: Vec<Span<'static>>, text: &str, look: Look, width: usize) -> Vec<Line<'static>> {
    note(lead, &format!("{} {text}", look.marks().more), width)
}

/// A search's matches under the files they are in: each file's path with
/// its directory dim, then its lines, numbered dim, a match in the output's
/// tone and a context line dim, the indent the file's lines share taken
/// off. A preview shows the first [`CARD_MATCHES`] matches, within
/// [`CARD_MATCH_LINES`] lines; expanded, every line shows, within
/// [`EXPANDED_ROWS`] rows. The rest is counted: as matches when only
/// matches are left out, else as lines. Returns the rows and whether any
/// line was left out.
pub(crate) fn matches(
    output: &[String],
    width: usize,
    look: Look,
    lead: &dyn Fn() -> Vec<Span<'static>>,
    detail: Detail,
) -> (Vec<Line<'static>>, bool) {
    let lines = cleaned(output);
    let found: Vec<Option<GrepLine>> = lines.iter().map(|line| grep_line(line)).collect();
    // A line in no shape counts as a match, so it is never left out for
    // matches after it.
    let is_match = |entry: &Option<GrepLine>| entry.is_none_or(|found| found.matched);
    let mut end = 0;
    if detail == Detail::Full {
        end = found.len();
    } else {
        let mut shown_matches = 0;
        for entry in &found {
            if end == CARD_MATCH_LINES || (is_match(entry) && shown_matches == CARD_MATCHES) {
                break;
            }
            shown_matches += usize::from(is_match(entry));
            end += 1;
        }
        if found.len() == end + 1 {
            end += 1;
        }
    }
    let digits = found[..end]
        .iter()
        .flatten()
        .map(|found| found.number.len())
        .max()
        .unwrap_or(1);
    let base = output_tone(look.tones);
    let bound = match detail {
        Detail::Preview => usize::MAX,
        Detail::Full => EXPANDED_ROWS,
    };
    let mut drawn = Vec::new();
    let mut at = 0;
    'groups: while at < end {
        // Expanded, the lines past the bound are counted with the lines
        // after them.
        if drawn.len() >= bound {
            end = at;
            break;
        }
        let Some(first) = found[at] else {
            drawn.extend(row(&lines[at], &[], base, width, (lead(), lead()), detail));
            at += 1;
            continue;
        };
        let same_file = found[at..end]
            .iter()
            .take_while(|entry| entry.is_some_and(|found| found.path == first.path))
            .count();
        let group: Vec<GrepLine> = found[at..at + same_file]
            .iter()
            .flatten()
            .copied()
            .collect();
        drawn.extend(row(
            first.path,
            &path_runs(first.path),
            base,
            width,
            (lead(), lead()),
            detail,
        ));
        let indent = group
            .iter()
            .filter(|found| !found.text.trim().is_empty())
            .map(|found| found.text.len() - found.text.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        for found in group {
            if drawn.len() >= bound {
                end = at;
                break 'groups;
            }
            let mut number = lead();
            number.extend([
                pad(2),
                Span::styled(format!("{:>digits$}", found.number), dim()),
                pad(2),
            ]);
            let mut under = lead();
            under.push(pad(2 + digits + 2));
            let style = if found.matched { base } else { dim() };
            let text = found.text.get(indent..).unwrap_or_default();
            drawn.extend(row(text, &[], style, width, (number, under), detail));
            at += 1;
        }
    }
    // One long line can pass the bound by itself.
    clip_rows(&mut drawn, bound);
    let left = &found[end..];
    if !left.is_empty() {
        let left_matches = left
            .iter()
            .filter(|entry| entry.is_some_and(|found| found.matched))
            .count();
        let text = if left_matches == left.len() {
            left_out(left_matches, copy::MATCH_NOUNS)
        } else {
            left_out(left.len(), copy::LINE_NOUNS)
        };
        drawn.extend(more(lead(), &text, look, width));
    }
    (drawn, !left.is_empty())
}

/// Entries one a line, each drawn by `draw`: a preview's first
/// [`RESULT_LINES`], or expanded every one within [`EXPANDED_ROWS`] rows,
/// then a count of the rest. Returns the rows and whether any entry was
/// left out.
fn entries(
    entries: &[String],
    width: usize,
    look: Look,
    lead: &dyn Fn() -> Vec<Span<'static>>,
    detail: Detail,
    draw: impl Fn(&str) -> Vec<Line<'static>>,
) -> (Vec<Line<'static>>, bool) {
    let count = |left: usize| more(lead(), &format!("{left} {}", copy::MORE), look, width);
    match detail {
        Detail::Preview => {
            let end = shown(entries.len(), RESULT_LINES);
            let mut drawn: Vec<Line<'static>> = entries[..end]
                .iter()
                .flat_map(|entry| draw(entry))
                .collect();
            let left = entries.len() - end;
            if left > 0 {
                drawn.extend(count(left));
            }
            (drawn, left > 0)
        }
        Detail::Full => bounded(entries.len(), EXPANDED_ROWS, |i| draw(&entries[i]), count),
    }
}

/// A listing's entries, each a path with its directory dim: the head of a
/// list reads better than its two ends.
pub(crate) fn listing(
    output: &[String],
    width: usize,
    look: Look,
    lead: &dyn Fn() -> Vec<Span<'static>>,
    detail: Detail,
) -> (Vec<Line<'static>>, bool) {
    let base = output_tone(look.tones);
    entries(&cleaned(output), width, look, lead, detail, |entry| {
        row(
            entry,
            &path_runs(entry),
            base,
            width,
            (lead(), lead()),
            detail,
        )
    })
}

/// A web search's results, each read from a line of its title, a space,
/// and its address: the title, then the address's host dim. A line in
/// another shape, such as an answer's prose, is drawn as it is.
pub(crate) fn results(
    output: &[String],
    width: usize,
    look: Look,
    lead: &dyn Fn() -> Vec<Span<'static>>,
    detail: Detail,
) -> (Vec<Line<'static>>, bool) {
    let base = output_tone(look.tones);
    entries(&cleaned(output), width, look, lead, detail, |entry| {
        let (title, url) = entry.rsplit_once(' ').unwrap_or(("", entry));
        let title = title.trim();
        let (text, runs) = match host(url) {
            Some(host) if !title.is_empty() => {
                let text = format!("{title}  {host}");
                let runs = vec![(title.len() + 2..text.len(), dim())];
                (text, runs)
            }
            Some(host) => (host.to_owned(), Vec::new()),
            None => (entry.to_owned(), Vec::new()),
        };
        row(&text, &runs, base, width, (lead(), lead()), detail)
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::transcript::RAIL;

    const PLAIN: Look = Look {
        tones: Tones::None,
        classic: false,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };

    fn lead() -> Vec<Span<'static>> {
        vec![pad(RAIL), Span::raw("│ ")]
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    }

    fn owned(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn grep_lines_split_into_path_number_and_text() {
        assert_eq!(
            grep_line("src/a.rs:12: let x = 1;"),
            Some(GrepLine {
                path: "src/a.rs",
                number: "12",
                text: "let x = 1;",
                matched: true,
            })
        );
        let context = grep_line("src/a.rs-11-   fn a() {").unwrap();
        assert_eq!(
            (context.path, context.number, context.text, context.matched),
            ("src/a.rs", "11", "  fn a() {", false)
        );
        // A path may hold colons and dashes; the text may hold anything.
        let odd = grep_line("a:1:b-2.txt:5: x:9: y").unwrap();
        assert_eq!(
            (odd.path, odd.number, odd.text),
            ("a:1:b-2.txt", "5", "x:9: y")
        );
        // An empty last line loses its space to the trim of the result.
        assert_eq!(grep_line("a.rs:3:").map(|found| found.text), Some(""));
        for other in [
            "No matches found",
            ":12: x",
            "a.rs:x: y",
            "a.rs:12-x",
            "",
            "[1 limit]",
        ] {
            assert_eq!(grep_line(other), None, "{other}");
        }
    }

    #[test]
    fn context_lines_are_never_counted_as_matches() {
        let lines = owned(&[
            "src/a.rs-11- before",
            "src/a.rs:12: hit",
            "src/a.rs-13- after",
            "src/b.rs-1- before",
            "src/b.rs:2: hit",
        ]);
        assert_eq!(grep_counts(&lines), Some((2, 2)));
        assert_eq!(grep_counts(&owned(&["src/a.rs-1- only context"])), None);
        assert_eq!(grep_counts(&owned(&["No matches found"])), None);
    }

    #[test]
    fn matches_group_under_their_files_and_the_rest_is_counted() {
        let output = owned(&[
            "src/controller.ts:45:   commands.register(open);",
            "src/controller.ts:52:   commands.register(close);",
            "src/menu.ts:7: commands.register(menu);",
            "src/menu.ts:9: commands.register(help);",
            "src/view.ts:3: commands.register(view);",
            "src/view.ts:4: commands.register(edit);",
        ]);
        let drawn = text(&matches(&output, 60, PLAIN, &lead, Detail::Preview).0);
        assert_eq!(
            drawn,
            [
                "  │ src/controller.ts",
                "  │   45  commands.register(open);",
                "  │   52  commands.register(close);",
                "  │ src/menu.ts",
                "  │    7  commands.register(menu);",
                "  │    9  commands.register(help);",
                "  │ ⋯ 2 more matches",
            ]
        );
        // One left out is drawn rather than counted.
        let five = text(&matches(&output[..5], 60, PLAIN, &lead, Detail::Preview).0);
        assert_eq!(five.last().unwrap(), "  │    3  commands.register(view);");
        // The directory is dim, the name is not.
        let lines = matches(&output, 60, PLAIN, &lead, Detail::Preview).0;
        let header = &lines[0];
        assert_eq!(header.spans[2].content, "src/");
        assert_eq!(header.spans[2].style, dim());
        assert_eq!(header.spans[3].content, "controller.ts");
    }

    #[test]
    fn context_lines_are_dim_and_bound_the_card_by_lines() {
        let output: Vec<String> = (0..4)
            .flat_map(|n| {
                let at = 10 * (n + 1);
                [
                    format!("a.rs-{}- before {n}", at - 1),
                    format!("a.rs:{at}: hit {n}"),
                    format!("a.rs-{}- after {n}", at + 1),
                ]
            })
            .collect();
        let lines = matches(&output, 60, PLAIN, &lead, Detail::Preview).0;
        let drawn = text(&lines);
        assert_eq!(drawn.len(), 1 + CARD_MATCH_LINES + 1, "{drawn:#?}");
        assert_eq!(drawn[1], "  │    9  before 0");
        assert_eq!(drawn[2], "  │   10  hit 0");
        // Context is left out with the matches, so lines are counted.
        assert_eq!(drawn.last().unwrap(), "  │ ⋯ 4 more lines");
        let context = &lines[1];
        assert_eq!(context.spans.last().unwrap().style, dim());
    }

    #[test]
    fn a_line_in_no_shape_is_drawn_as_it_is() {
        let output = owned(&["a.rs:1: x", "something else", "a.rs:2: y"]);
        assert_eq!(
            text(&matches(&output, 40, PLAIN, &lead, Detail::Preview).0),
            [
                "  │ a.rs",
                "  │   1  x",
                "  │ something else",
                "  │ a.rs",
                "  │   2  y",
            ]
        );
    }

    #[test]
    fn a_listing_draws_its_first_entries_then_counts_the_rest() {
        let output: Vec<String> = (0..23).map(|n| format!("tests/t{n}.test.ts")).collect();
        let drawn = text(&listing(&output, 40, PLAIN, &lead, Detail::Preview).0);
        assert_eq!(
            drawn,
            [
                "  │ tests/t0.test.ts",
                "  │ tests/t1.test.ts",
                "  │ tests/t2.test.ts",
                "  │ tests/t3.test.ts",
                "  │ ⋯ 19 more",
            ]
        );
        assert_eq!(
            listing(&output[..5], 40, PLAIN, &lead, Detail::Preview)
                .0
                .len(),
            5
        );
        // A directory keeps its own slash with its name.
        assert_eq!(path_runs("src/bin/"), vec![(0..4, dim())]);
        assert_eq!(path_runs("bin/"), Vec::new());
    }

    #[test]
    fn web_results_read_as_titles_with_their_hosts_dim() {
        let output = owned(&[
            "Wrapping text in ratatui https://docs.rs/ratatui/latest/ratatui/widgets/struct.Wrap.html",
            " https://www.example.com/a",
            "An answer in prose.",
        ]);
        let lines = results(&output, 60, PLAIN, &lead, Detail::Preview).0;
        assert_eq!(
            text(&lines),
            [
                "  │ Wrapping text in ratatui  docs.rs",
                "  │ example.com",
                "  │ An answer in prose.",
            ]
        );
        let shown = lines[0].spans.last().unwrap();
        assert_eq!((shown.content.as_ref(), shown.style), ("docs.rs", dim()));
        assert_eq!(host("HTTPS://user@www.a.test:8080/x"), Some("a.test:8080"));
        assert_eq!(host("ftp://a.test"), None);
        assert_eq!(host("https://"), None);
    }

    #[test]
    fn expanded_every_entry_and_line_shows_within_the_bound() {
        let output: Vec<String> = (0..23).map(|n| format!("tests/t{n}.test.ts")).collect();
        let (drawn, left_out) = listing(&output, 40, PLAIN, &lead, Detail::Full);
        assert_eq!(drawn.len(), 23);
        assert!(!left_out);
        // A long entry wraps in full rather than being cut.
        let long = vec![format!("src/{}.rs", "a".repeat(200))];
        let (drawn, _) = listing(&long, 40, PLAIN, &lead, Detail::Full);
        assert!(drawn.len() > RESULT_LINE_ROWS, "{}", drawn.len());
        assert!(!text(&drawn).concat().contains('…'));
        // Every match and context line shows.
        let found: Vec<String> = (1..=30)
            .flat_map(|n| {
                [
                    format!("a.rs-{}- ctx", 2 * n),
                    format!("a.rs:{}: hit", 2 * n + 1),
                ]
            })
            .collect();
        let (drawn, left_out) = matches(&found, 60, PLAIN, &lead, Detail::Full);
        assert_eq!(drawn.len(), 1 + 60);
        assert!(!left_out);
        // A huge listing stops at the bound and counts the rest.
        let many: Vec<String> = (0..1000).map(|n| format!("f{n}")).collect();
        let (drawn, left_out) = listing(&many, 40, PLAIN, &lead, Detail::Full);
        assert!(left_out);
        assert_eq!(drawn.len(), EXPANDED_ROWS + 1);
        assert_eq!(
            text(&drawn).last().unwrap(),
            &format!("  │ ⋯ {} more", 1000 - EXPANDED_ROWS)
        );
        let hits: Vec<String> = (0..1000).map(|n| format!("a.rs:{n}: hit")).collect();
        let (drawn, left_out) = matches(&hits, 60, PLAIN, &lead, Detail::Full);
        assert!(left_out);
        assert!(drawn.len() <= EXPANDED_ROWS + 1, "{}", drawn.len());
        let counted = text(&drawn).last().unwrap().clone();
        let shown = drawn.len() - 2;
        assert_eq!(counted, format!("  │ ⋯ {} more matches", 1000 - shown));
    }

    #[test]
    fn sizes_and_counts_read_in_words() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(12_400), "12.4 KB");
        assert_eq!(size(3_100_000), "3.1 MB");
        assert_eq!(count(1, copy::MATCH_NOUNS), "1 match");
        assert_eq!(count(5, copy::MATCH_NOUNS), "5 matches");
    }

    /// Every drawn line fits `width` and carries no control character.
    fn sound(lines: &[Line], width: usize) -> Result<(), TestCaseError> {
        for line in lines {
            prop_assert!(line.width() <= width, "{line:?} > {width}");
            for span in &line.spans {
                prop_assert!(!span.content.contains(char::is_control), "{span:?}");
            }
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        // Any line reads without a panic, and a line that reads is exactly
        // its parts put back together.
        #[test]
        fn a_read_grep_line_is_its_parts(line in "[a-z0-9:/. \\-]{0,40}|.{0,40}") {
            if let Some(found) = grep_line(&line) {
                let separator = if found.matched { ':' } else { '-' };
                let rebuilt = format!("{}{separator}{}{separator}", found.path, found.number);
                prop_assert!(!found.path.is_empty());
                prop_assert!(!found.number.is_empty() && found.number.bytes().all(|b| b.is_ascii_digit()));
                prop_assert!(line == rebuilt || line == format!("{rebuilt} {}", found.text), "{line:?}");
            }
        }

        // Lines the tool writes read back as written, and only its matches
        // are counted, in as many files as they name.
        #[test]
        fn written_grep_lines_read_back(
            written in prop::collection::vec(("[a-z_./]{1,12}", 1u32..100_000, ".{0,30}", any::<bool>()), 1..12),
        ) {
            let lines: Vec<String> = written
                .iter()
                .map(|(path, number, text, matched)| {
                    let separator = if *matched { ':' } else { '-' };
                    format!("{path}{separator}{number}{separator} {text}")
                })
                .collect();
            for (line, (path, number, text, matched)) in lines.iter().zip(&written) {
                let number = number.to_string();
                let expected = GrepLine {
                    path: path.as_str(),
                    number: number.as_str(),
                    text: text.as_str(),
                    matched: *matched,
                };
                prop_assert_eq!(grep_line(line), Some(expected));
            }
            let matched: Vec<_> = written.iter().filter(|line| line.3).collect();
            let files: HashSet<&String> = matched.iter().map(|line| &line.0).collect();
            let expected = (!matched.is_empty()).then_some((matched.len(), files.len()));
            prop_assert_eq!(grep_counts(&lines), expected);
        }

        // Any output draws in every body without a panic, inside its width,
        // with no control character, and never more rows than its bound.
        #[test]
        fn any_output_draws_inside_its_width(
            output in prop::collection::vec("[a-z:/. \\-0-9\u{1b}\t\\[]{0,50}|.{0,50}", 0..14),
            width in 20usize..90,
        ) {
            for look in [PLAIN, Look { tones: Tones::TrueColor, ..PLAIN }, Look { classic: true, ..PLAIN }] {
                let drawn = matches(&output, width, look, &lead, Detail::Preview).0;
                sound(&drawn, width)?;
                // Each shown line takes at most its row bound, and so may
                // the header of each file.
                prop_assert!(drawn.len() <= 2 * (CARD_MATCH_LINES + 1) * RESULT_LINE_ROWS + 1);
                let drawn = listing(&output, width, look, &lead, Detail::Preview).0;
                sound(&drawn, width)?;
                prop_assert!(drawn.len() <= (RESULT_LINES + 1) * RESULT_LINE_ROWS + 1);
                let drawn = results(&output, width, look, &lead, Detail::Preview).0;
                sound(&drawn, width)?;
                prop_assert!(drawn.len() <= (RESULT_LINES + 1) * RESULT_LINE_ROWS + 1);
                type Body = fn(&[String], usize, Look, &dyn Fn() -> Vec<Span<'static>>, Detail) -> (Vec<Line<'static>>, bool);
                for body in [matches as Body, listing, results] {
                    let drawn = body(&output, width, look, &lead, Detail::Full).0;
                    sound(&drawn, width)?;
                    prop_assert!(drawn.len() <= EXPANDED_ROWS + 1);
                }
            }
        }

        // A host is part of its address and holds no separator.
        #[test]
        fn a_host_is_inside_its_address(url in "(https?://)?[a-zA-Z0-9@:/?#. w]{0,30}|.{0,30}") {
            if let Some(found) = host(&url) {
                prop_assert!(url.contains(found));
                prop_assert!(!found.is_empty());
                prop_assert!(!found.contains(['/', '?', '#', '@']), "{found:?}");
            }
        }
    }
}
