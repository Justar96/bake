//! Where an `edit` whose old text is not in the file most likely meant,
//! added to its error so the model need not read the file again. Bake's
//! own; Pi's error stops at the rule that the text must match exactly.
//!
//! The file's lines and the old text's are compared with their leading and
//! trailing whitespace trimmed. The window of the file as long as the old
//! text with the most lines equal at the same offset is the closest; it is
//! shown when at least half of the old text's non-blank lines match there,
//! or, for an old text of one line, when a line of the file equals it once
//! runs of whitespace are collapsed. Files over [`MAX_FILE_BYTES`] and old
//! texts over [`MAX_OLD_LINES`] lines get no hint.

use std::collections::HashMap;

/// The largest file searched, in bytes.
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// The most lines of old text searched for.
pub const MAX_OLD_LINES: usize = 200;

/// The most lines of the file the hint shows.
pub const MAX_SHOWN_LINES: usize = 30;

/// The most characters of the file the hint shows.
pub const MAX_SHOWN_CHARS: usize = 2000;

/// The most characters of each side a differing line shows.
const MAX_DIFFERENCE_CHARS: usize = 120;

/// The closest text: its lines, 1-based, and how it differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closest {
    /// The first line of the window.
    pub first: usize,
    /// The last line of the window.
    pub last: usize,
    /// The lines shown, each with its number; a gap means lines left out.
    pub shown: Vec<(usize, String)>,
    /// The sentence naming the first difference.
    pub difference: String,
}

fn lines(text: &str) -> Vec<&str> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n').collect()
}

fn collapsed(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn without_whitespace(line: &str) -> String {
    line.chars().filter(|c| !c.is_whitespace()).collect()
}

fn cut(line: &str) -> String {
    if line.chars().count() <= MAX_DIFFERENCE_CHARS {
        line.to_owned()
    } else {
        let mut kept: String = line.chars().take(MAX_DIFFERENCE_CHARS).collect();
        kept.push('…');
        kept
    }
}

/// The closest text to `old_text` in `content`, both LF-normalized; `None`
/// when nothing is close enough or the bounds are passed.
pub fn closest_text(content: &str, old_text: &str) -> Option<Closest> {
    if content.len() > MAX_FILE_BYTES {
        return None;
    }
    let old = lines(old_text);
    if old.len() > MAX_OLD_LINES {
        return None;
    }
    let file = lines(content);
    let start = if old.len() == 1 {
        let wanted = collapsed(old[0]);
        if wanted.is_empty() {
            return None;
        }
        file.iter().position(|line| collapsed(line) == wanted)?
    } else {
        best_window(&file, &old)?
    };
    let end = (start + old.len()).min(file.len());
    let shown = shown_lines(&file, start, end)?;
    Some(Closest {
        first: start + 1,
        last: end,
        shown,
        difference: difference(&file, &old, start),
    })
}

/// The start of the window with the most trimmed lines equal at the same
/// offset, the earliest of equals, when at least half of the old text's
/// non-blank lines match there.
fn best_window(file: &[&str], old: &[&str]) -> Option<usize> {
    let mut offsets: HashMap<&str, Vec<usize>> = HashMap::new();
    for (offset, line) in old.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            offsets.entry(trimmed).or_default().push(offset);
        }
    }
    let non_blank: usize = offsets.values().map(Vec::len).sum();
    if non_blank == 0 {
        return None;
    }
    let mut scores: HashMap<usize, usize> = HashMap::new();
    for (index, line) in file.iter().enumerate() {
        for &offset in offsets.get(line.trim()).into_iter().flatten() {
            if let Some(start) = index.checked_sub(offset) {
                *scores.entry(start).or_default() += 1;
            }
        }
    }
    let (start, score) = scores
        .into_iter()
        .max_by(|(a_start, a_score), (b_start, b_score)| {
            a_score.cmp(b_score).then(b_start.cmp(a_start))
        })?;
    (score * 2 >= non_blank).then_some(start)
}

/// The window's lines within the bounds: from both ends inward, the rest
/// left out in the middle; `None` when not even one line fits.
fn shown_lines(file: &[&str], start: usize, end: usize) -> Option<Vec<(usize, String)>> {
    let mut head: Vec<usize> = Vec::new();
    let mut tail: Vec<usize> = Vec::new();
    let (mut low, mut high) = (start, end);
    let mut chars = 0;
    let mut from_head = true;
    while low < high && head.len() + tail.len() < MAX_SHOWN_LINES {
        let index = if from_head { low } else { high - 1 };
        let size = file[index].chars().count() + 1;
        if chars + size > MAX_SHOWN_CHARS {
            break;
        }
        chars += size;
        if from_head {
            head.push(index);
            low += 1;
        } else {
            tail.push(index);
            high -= 1;
        }
        from_head = !from_head;
    }
    if head.is_empty() {
        return None;
    }
    tail.reverse();
    Some(
        head.into_iter()
            .chain(tail)
            .map(|index| (index + 1, file[index].to_owned()))
            .collect(),
    )
}

/// The sentence naming the first difference between the old text and the
/// window at `start`.
fn difference(file: &[&str], old: &[&str], start: usize) -> String {
    for (offset, expected) in old.iter().enumerate() {
        let found = file.get(start + offset).copied();
        if found.map(without_whitespace) != Some(without_whitespace(expected)) {
            let found = match found {
                Some(found) => format!("\"{}\"", cut(found)),
                None => "the end of the file".to_owned(),
            };
            return format!(
                "Line {} differs: expected \"{}\", found {found}.",
                start + offset + 1,
                cut(expected)
            );
        }
    }
    "It differs only in whitespace (indentation, trailing spaces, or line endings).".to_owned()
}

/// The hint's text: where the closest text is, the lines in a fence, and
/// the first difference.
pub fn render(closest: &Closest) -> String {
    let fence_length = closest
        .shown
        .iter()
        .map(|(_, line)| longest_backtick_run(line) + 1)
        .max()
        .unwrap_or(0)
        .max(3);
    let fence = "`".repeat(fence_length);
    let mut out = vec![
        format!(
            "The closest text is at lines {}-{}:",
            closest.first, closest.last
        ),
        fence.clone(),
    ];
    let mut previous: Option<usize> = None;
    for (number, line) in &closest.shown {
        if let Some(previous) = previous
            && *number > previous + 1
        {
            out.push(format!("[... {} lines ...]", number - previous - 1));
        }
        out.push(line.clone());
        previous = Some(*number);
    }
    out.push(fence);
    out.push(closest.difference.clone());
    out.join("\n")
}

fn longest_backtick_run(line: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in line.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_whitespace_only_difference_is_named() {
        let content = "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";
        let old = "fn main() {\n  let x = 1;\n  println!(\"{x}\");\n}";
        let closest = closest_text(content, old).expect("closest");
        assert_eq!(
            render(&closest),
            "The closest text is at lines 1-4:\n```\nfn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n```\nIt differs only in whitespace (indentation, trailing spaces, or line endings)."
        );
    }

    #[test]
    fn one_changed_line_is_named() {
        let content = "a\nb\nfn f() {\n    one();\n    two();\n}\nz\n";
        let old = "fn f() {\n    one();\n    three();\n}";
        let closest = closest_text(content, old).expect("closest");
        assert_eq!(
            render(&closest),
            "The closest text is at lines 3-6:\n```\nfn f() {\n    one();\n    two();\n}\n```\nLine 5 differs: expected \"    three();\", found \"    two();\"."
        );
    }

    #[test]
    fn one_line_matches_with_whitespace_collapsed() {
        let closest = closest_text("x\nlet  a =   1;\ny\n", "let a = 1;").expect("closest");
        assert_eq!((closest.first, closest.last), (2, 2));
        assert_eq!(closest.shown, [(2, "let  a =   1;".to_owned())]);
        assert!(
            closest
                .difference
                .starts_with("It differs only in whitespace")
        );
        assert_eq!(closest_text("x\nlet a = 2;\n", "let a = 1;"), None);
    }

    #[test]
    fn nothing_similar_gives_no_hint() {
        assert_eq!(
            closest_text("alpha\nbeta\ngamma\n", "one\ntwo\nthree\nfour"),
            None
        );
        // Fewer than half the non-blank lines.
        assert_eq!(
            closest_text("one\nx\ny\nz\n", "one\ntwo\nthree\nfour"),
            None
        );
        assert_eq!(closest_text("a\n", "\n\n"), None);
    }

    #[test]
    fn the_bounds_hold() {
        let content: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let old: String = (1..=60)
            .map(|n| format!("line {n}\n"))
            .collect::<String>()
            .replace("line 30\n", "changed\n");
        let closest = closest_text(&content, &old).expect("closest");
        assert_eq!((closest.first, closest.last), (1, 60));
        assert_eq!(closest.shown.len(), MAX_SHOWN_LINES);
        let text = render(&closest);
        assert!(
            text.contains("line 15\n[... 30 lines ...]\nline 46"),
            "{text}"
        );
        assert!(text.ends_with("Line 30 differs: expected \"changed\", found \"line 30\"."));
        // Long lines: the characters bound.
        let long: String = (0..10)
            .map(|n| format!("{n}{}\n", "x".repeat(400)))
            .collect();
        let closest = closest_text(&long, &long.replace("5x", "5y")).expect("closest");
        let chars: usize = closest
            .shown
            .iter()
            .map(|(_, line)| line.chars().count() + 1)
            .sum();
        assert!(chars <= MAX_SHOWN_CHARS && closest.shown.len() < 10);
        // Too large a file or old text: no hint.
        let huge = "a\n".repeat(MAX_FILE_BYTES / 2 + 1);
        assert_eq!(closest_text(&huge, "a\nb"), None);
        let many = "a\n".repeat(MAX_OLD_LINES + 1);
        assert_eq!(closest_text("a\n", &many), None);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn a_hint_shows_lines_of_the_file_within_the_bounds(
            content in "([a-c `\t]{0,6}\n){0,40}",
            old in "([a-c `\t]{0,6}\n){0,8}[a-c ]{0,6}",
        ) {
            if let Some(closest) = closest_text(&content, &old) {
                let file = lines(&content);
                prop_assert!(closest.first >= 1 && closest.first <= closest.last.max(1));
                prop_assert!(closest.shown.len() <= MAX_SHOWN_LINES);
                let chars: usize = closest.shown.iter().map(|(_, line)| line.chars().count() + 1).sum();
                prop_assert!(chars <= MAX_SHOWN_CHARS);
                for (number, line) in &closest.shown {
                    prop_assert!(*number >= closest.first && *number <= closest.last);
                    prop_assert_eq!(Some(&line.as_str()), file.get(number - 1));
                }
                let text = render(&closest);
                prop_assert!(text.starts_with("The closest text is at lines "));
            }
        }

        #[test]
        fn any_text_is_safe(content in "\\PC{0,200}", old in "\\PC{0,80}") {
            if let Some(closest) = closest_text(&content, &old) {
                let _ = render(&closest);
            }
        }
    }
}
