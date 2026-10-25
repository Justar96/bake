//! An edit's diff as the transcript draws it: lines numbered from their hunk
//! headers, the lines a hunk skips counted, a removed run paired with the
//! added run after it, and in each pair the part that changed.
//!
//! Input is unified-diff text, one line each: `@@ -12,3 +12,4 @@` opens a
//! hunk, and every other line starts with its sign, ` `, `-`, or `+`. Lines
//! before the first hunk header count from 1.

use std::ops::Range;

/// Which side of the change a line is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Context,
    Removed,
    Added,
}

/// One line of the diff, with its numbers in the old and new file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub change: Change,
    pub old: Option<usize>,
    pub new: Option<usize>,
    /// The line's text without its sign.
    pub code: String,
    /// The bytes of `code` that differ from the line it pairs with.
    pub emphasis: Option<Range<usize>>,
}

impl DiffLine {
    /// The number shown in a single column: the old file's for a removed
    /// line, the new file's otherwise.
    pub fn number(&self) -> Option<usize> {
        match self.change {
            Change::Removed => self.old,
            _ => self.new,
        }
    }
}

/// A row of the diff: a line, or a count of lines the hunks leave out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Line(DiffLine),
    /// Unmodified lines between the start of the file or the previous hunk
    /// and the next hunk.
    Skipped(usize),
}

/// The numbers a hunk header opens at: `@@ -12,3 +14,4 @@` gives (12, 14).
fn hunk_start(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.strip_prefix("@@")?.split_whitespace();
    let number = |part: Option<&str>, sign: char| -> Option<usize> {
        part?.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    Some((number(parts.next(), '-')?, number(parts.next(), '+')?))
}

/// Parses `output` into numbered rows, with the changed part of each paired
/// removed and added line.
pub fn parse(output: &[String]) -> Vec<DiffRow> {
    let (mut old, mut new) = (1, 1);
    let mut rows = Vec::new();
    for line in output {
        if let Some((from_old, from_new)) = hunk_start(line) {
            if from_old > old {
                rows.push(DiffRow::Skipped(from_old - old));
            }
            (old, new) = (from_old, from_new);
            continue;
        }
        let (change, code) = match line.as_bytes().first() {
            Some(b'-') => (Change::Removed, &line[1..]),
            Some(b'+') => (Change::Added, &line[1..]),
            Some(b' ') => (Change::Context, &line[1..]),
            _ => (Change::Context, line.as_str()),
        };
        let (o, n) = match change {
            Change::Context => (Some(old), Some(new)),
            Change::Removed => (Some(old), None),
            Change::Added => (None, Some(new)),
        };
        old += usize::from(o.is_some());
        new += usize::from(n.is_some());
        rows.push(DiffRow::Line(DiffLine {
            change,
            old: o,
            new: n,
            code: code.to_owned(),
            emphasis: None,
        }));
    }
    emphasize(&mut rows);
    rows
}

/// Marks what changed between each removed line and the added line it
/// pairs with, by the text they share at their start and end.
fn emphasize(rows: &mut [DiffRow]) {
    for (left, right) in pair_indices(rows) {
        let (Some(l), Some(r)) = (left, right) else {
            continue;
        };
        let (DiffRow::Line(a), DiffRow::Line(b)) = (&rows[l], &rows[r]) else {
            continue;
        };
        if let Some((ea, eb)) = changed(&a.code, &b.code) {
            if let DiffRow::Line(a) = &mut rows[l] {
                a.emphasis = Some(ea);
            }
            if let DiffRow::Line(b) = &mut rows[r] {
                b.emphasis = Some(eb);
            }
        }
    }
}

/// The differing middle of `a` and `b` after their common prefix and
/// suffix, on character boundaries; `None` when they share nothing but
/// whitespace, since then the whole line changed.
pub fn changed(a: &str, b: &str) -> Option<(Range<usize>, Range<usize>)> {
    let prefix: usize = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    let suffix: usize = a[prefix..]
        .chars()
        .rev()
        .zip(b[prefix..].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    let shared = a[..prefix].trim().len() + a[a.len() - suffix..].trim().len();
    if shared == 0 {
        return None;
    }
    Some((prefix..a.len() - suffix, prefix..b.len() - suffix))
}

/// One row of a side-by-side diff: the lines on each side, or a count of
/// lines left out, which spans both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitRow<'a> {
    Pair(Option<&'a DiffLine>, Option<&'a DiffLine>),
    Skipped(usize),
}

/// Indices of the rows each split row shows: a context line on both sides,
/// and a run of removed lines beside the run of added lines that follows
/// it. `None` leaves that side empty.
fn pair_indices(rows: &[DiffRow]) -> Vec<(Option<usize>, Option<usize>)> {
    let change = |i: usize| match &rows[i] {
        DiffRow::Line(line) => Some(line.change),
        DiffRow::Skipped(_) => None,
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let removed = (i..rows.len())
            .take_while(|&k| change(k) == Some(Change::Removed))
            .count();
        let added = (i + removed..rows.len())
            .take_while(|&k| change(k) == Some(Change::Added))
            .count();
        if removed + added == 0 {
            if change(i).is_some() {
                out.push((Some(i), Some(i)));
            }
            i += 1;
            continue;
        }
        for k in 0..removed.max(added) {
            out.push((
                (k < removed).then_some(i + k),
                (k < added).then_some(i + removed + k),
            ));
        }
        i += removed + added;
    }
    out
}

/// The rows of a side-by-side diff.
pub fn split(rows: &[DiffRow]) -> Vec<SplitRow<'_>> {
    let line = |i: Option<usize>| match i.map(|i| &rows[i]) {
        Some(DiffRow::Line(line)) => Some(line),
        _ => None,
    };
    let pairs = pair_indices(rows);
    let mut out = Vec::new();
    let mut next = pairs.iter().peekable();
    for (index, row) in rows.iter().enumerate() {
        if let DiffRow::Skipped(count) = row {
            out.push(SplitRow::Skipped(*count));
            continue;
        }
        // Emit each pair once, when its first line comes up.
        while let Some(&&(l, r)) = next.peek() {
            if l.or(r).is_some_and(|first| first <= index) {
                out.push(SplitRow::Pair(line(l), line(r)));
                next.next();
            } else {
                break;
            }
        }
    }
    out
}

/// Digits the widest line number takes.
pub fn number_width(rows: &[DiffRow]) -> usize {
    rows.iter()
        .filter_map(|row| match row {
            DiffRow::Line(line) => line.old.max(line.new),
            DiffRow::Skipped(_) => None,
        })
        .max()
        .map_or(1, |n| n.to_string().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|l| (*l).to_owned()).collect()
    }

    fn line(row: &DiffRow) -> &DiffLine {
        match row {
            DiffRow::Line(line) => line,
            DiffRow::Skipped(n) => panic!("skipped {n}"),
        }
    }

    #[test]
    fn hunk_headers_number_the_lines_and_count_what_they_skip() {
        let rows = parse(&lines(&[
            "@@ -12,3 +12,4 @@",
            " a",
            "-b",
            "+B",
            "+c",
            " d",
            "@@ -30,1 +31,1 @@ fn tail()",
            " z",
        ]));
        assert_eq!(rows[0], DiffRow::Skipped(11));
        let numbers: Vec<_> = rows[1..6]
            .iter()
            .map(|r| (line(r).old, line(r).new))
            .collect();
        assert_eq!(
            numbers,
            [
                (Some(12), Some(12)),
                (Some(13), None),
                (None, Some(13)),
                (None, Some(14)),
                (Some(14), Some(15))
            ]
        );
        assert_eq!(rows[6], DiffRow::Skipped(15));
        assert_eq!(
            (line(&rows[7]).old, line(&rows[7]).new),
            (Some(30), Some(31))
        );
        assert_eq!(line(&rows[2]).number(), Some(13));
        assert_eq!(line(&rows[1]).code, "a");
        assert_eq!(number_width(&rows), 2);
    }

    #[test]
    fn without_a_header_lines_count_from_one() {
        let rows = parse(&lines(&[" a", "-b", "+c"]));
        assert_eq!(line(&rows[0]).new, Some(1));
        assert_eq!(line(&rows[1]).old, Some(2));
        assert_eq!(line(&rows[2]).new, Some(2));
        assert_eq!(number_width(&parse(&[])), 1);
    }

    #[test]
    fn a_pair_marks_only_what_changed() {
        let rows = parse(&lines(&[
            "-  if (quote) fields.push(rest);",
            r#"+  if (quote) throw new SyntaxError("unterminated quote");"#,
        ]));
        let (a, b) = (line(&rows[0]), line(&rows[1]));
        assert_eq!(&a.code[a.emphasis.clone().unwrap()], "fields.push(rest");
        assert_eq!(
            &b.code[b.emphasis.clone().unwrap()],
            r#"throw new SyntaxError("unterminated quote""#
        );
        // Lines that share only whitespace changed whole; a lone line has
        // nothing to compare with.
        assert_eq!(changed("  abc", "  xyz"), None);
        assert_eq!(changed("界a", "界b"), Some((3..4, 3..4)));
        let lone = parse(&lines(&["+new"]));
        assert_eq!(line(&lone[0]).emphasis, None);
    }

    #[test]
    fn split_rows_pair_runs_and_span_skips() {
        let rows = parse(&lines(&[
            "@@ -5,4 +5,3 @@",
            " a",
            "-b",
            "-c",
            "+B",
            " d",
            "+e",
        ]));
        let shape: Vec<String> = split(&rows)
            .iter()
            .map(|row| match row {
                SplitRow::Skipped(n) => format!("skip {n}"),
                SplitRow::Pair(l, r) => format!(
                    "{}|{}",
                    l.map_or("", |l| l.code.as_str()),
                    r.map_or("", |r| r.code.as_str())
                ),
            })
            .collect();
        assert_eq!(shape, ["skip 4", "a|a", "b|B", "c|", "d|d", "|e"]);
    }
}
