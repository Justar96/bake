//! The line diff and unified patch Pi's edit tool takes from jsdiff 8.0.4
//! (`diffLines`, `structuredPatch`, `formatPatch`, `createTwoFilesPatch`;
//! BSD-3-Clause, see the crate's `NOTICE`), ported so the diff and patch in
//! an edit result match Pi's byte for byte.
//!
//! The algorithm is jsdiff's Myers variant, including its diagonal pruning
//! and tie-breaking; an `O((N + M) D)` search with no edit-length limit, as
//! Pi calls it.

use std::rc::Rc;

/// One run of a diff: common, added, or removed lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    pub value: String,
    pub added: bool,
    pub removed: bool,
}

struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<Rc<Component>>,
}

impl Drop for Component {
    // Long chains would overflow the stack if dropped recursively.
    fn drop(&mut self) {
        let mut next = self.previous.take();
        while let Some(component) = next {
            match Rc::try_unwrap(component) {
                Ok(mut owned) => next = owned.previous.take(),
                Err(_) => break,
            }
        }
    }
}

#[derive(Clone)]
struct DiffPath {
    old_pos: isize,
    last: Option<Rc<Component>>,
}

/// jsdiff's line `tokenize`: each line with its `\n` or `\r\n`; a final
/// empty line is dropped, and so are empty tokens.
fn tokenize(value: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    for (index, _) in value.match_indices('\n') {
        tokens.push(&value[start..=index]);
        start = index + 1;
    }
    if start < value.len() {
        tokens.push(&value[start..]);
    }
    tokens
}

fn add_to_path(path: &DiffPath, added: bool, removed: bool, old_pos_inc: isize) -> DiffPath {
    let last = match &path.last {
        Some(last) if last.added == added && last.removed == removed => Rc::new(Component {
            count: last.count + 1,
            added,
            removed,
            previous: last.previous.clone(),
        }),
        last => Rc::new(Component {
            count: 1,
            added,
            removed,
            previous: last.clone(),
        }),
    };
    DiffPath {
        old_pos: path.old_pos + old_pos_inc,
        last: Some(last),
    }
}

fn extract_common(path: &mut DiffPath, new: &[&str], old: &[&str], diagonal: isize) -> isize {
    let (new_len, old_len) = (new.len() as isize, old.len() as isize);
    let mut old_pos = path.old_pos;
    let mut new_pos = old_pos - diagonal;
    let mut common = 0;
    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && old[(old_pos + 1) as usize] == new[(new_pos + 1) as usize]
    {
        new_pos += 1;
        old_pos += 1;
        common += 1;
    }
    if common > 0 {
        path.last = Some(Rc::new(Component {
            count: common,
            added: false,
            removed: false,
            previous: path.last.take(),
        }));
    }
    path.old_pos = old_pos;
    new_pos
}

fn build_values(last: Option<Rc<Component>>, new: &[&str], old: &[&str]) -> Vec<Change> {
    let mut components = Vec::new();
    let mut next = last;
    while let Some(component) = next {
        next = component.previous.clone();
        components.push((component.count, component.added, component.removed));
    }
    components.reverse();
    let (mut new_pos, mut old_pos) = (0usize, 0usize);
    components
        .into_iter()
        .map(|(count, added, removed)| {
            let value = if removed {
                let value = old[old_pos..old_pos + count].concat();
                old_pos += count;
                value
            } else {
                let value = new[new_pos..new_pos + count].concat();
                new_pos += count;
                if !added {
                    old_pos += count;
                }
                value
            };
            Change {
                value,
                added,
                removed,
            }
        })
        .collect()
}

/// jsdiff's `diffLines(old, new)` with no options.
pub(crate) fn diff_lines(old: &str, new: &str) -> Vec<Change> {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    let (old_len, new_len) = (old_tokens.len() as isize, new_tokens.len() as isize);
    let max_edit_length = new_len + old_len;
    let offset = max_edit_length + 1;
    let mut best: Vec<Option<DiffPath>> = vec![None; (2 * offset + 1) as usize];
    let slot = |diagonal: isize| (diagonal + offset) as usize;

    let mut first = DiffPath {
        old_pos: -1,
        last: None,
    };
    let new_pos = extract_common(&mut first, &new_tokens, &old_tokens, 0);
    if first.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        return build_values(first.last, &new_tokens, &old_tokens);
    }
    best[slot(0)] = Some(first);

    let mut min_diagonal = isize::MIN;
    let mut max_diagonal = isize::MAX;
    let mut edit_length: isize = 1;
    while edit_length <= max_edit_length {
        let mut diagonal = min_diagonal.max(-edit_length);
        while diagonal <= max_diagonal.min(edit_length) {
            let remove_path = best[slot(diagonal - 1)].take();
            let add_path = best[slot(diagonal + 1)].clone();
            let can_add = add_path.as_ref().is_some_and(|path| {
                let add_new_pos = path.old_pos - diagonal;
                0 <= add_new_pos && add_new_pos < new_len
            });
            let can_remove = remove_path
                .as_ref()
                .is_some_and(|path| path.old_pos + 1 < old_len);
            if !can_add && !can_remove {
                best[slot(diagonal)] = None;
                diagonal += 2;
                continue;
            }
            // Branch from the path farthest along the old text that stays
            // inside the edit graph; ties go to the removal.
            let use_add = !can_remove
                || (can_add
                    && remove_path.as_ref().map(|path| path.old_pos)
                        < add_path.as_ref().map(|path| path.old_pos));
            let mut base = match (use_add, &add_path, &remove_path) {
                (true, Some(add), _) => add_to_path(add, true, false, 0),
                (false, _, Some(remove)) => add_to_path(remove, false, true, 1),
                // `can_add` or `can_remove` holds, so the chosen path exists.
                _ => break,
            };
            let new_pos = extract_common(&mut base, &new_tokens, &old_tokens, diagonal);
            if base.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return build_values(base.last, &new_tokens, &old_tokens);
            }
            if base.old_pos + 1 >= old_len {
                max_diagonal = max_diagonal.min(diagonal - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal = min_diagonal.max(diagonal + 1);
            }
            best[slot(diagonal)] = Some(base);
            diagonal += 2;
        }
        edit_length += 1;
    }
    // Unreachable: an edit script of length N + M always exists.
    Vec::new()
}

/// jsdiff's `splitLines`: lines with their `\n`.
fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = text.split('\n').map(|line| format!("{line}\n")).collect();
    if text.ends_with('\n') {
        lines.pop();
    } else if let Some(last) = lines.last_mut() {
        last.pop();
    }
    lines
}

struct Hunk {
    old_start: usize,
    old_lines: usize,
    new_start: usize,
    new_lines: usize,
    lines: Vec<String>,
}

/// jsdiff's `createTwoFilesPatch(name, name, old, new, undefined,
/// undefined, { context, headerOptions: FILE_HEADERS_ONLY })`.
pub(crate) fn create_two_files_patch(
    old_name: &str,
    new_name: &str,
    old: &str,
    new: &str,
    context: usize,
) -> String {
    let diff = diff_lines(old, new);
    let mut entries: Vec<(Vec<String>, bool, bool)> = diff
        .iter()
        .map(|change| (split_lines(&change.value), change.added, change.removed))
        .collect();
    entries.push((Vec::new(), false, false));
    let context_lines =
        |lines: &[String]| -> Vec<String> { lines.iter().map(|line| format!(" {line}")).collect() };

    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old_range_start, mut new_range_start) = (0usize, 0usize);
    let mut current: Vec<String> = Vec::new();
    let (mut old_line, mut new_line) = (1usize, 1usize);
    for index in 0..entries.len() {
        let (lines, added, removed) = (&entries[index].0, entries[index].1, entries[index].2);
        if added || removed {
            if old_range_start == 0 {
                old_range_start = old_line;
                new_range_start = new_line;
                if index > 0 {
                    let previous = &entries[index - 1].0;
                    current = if context > 0 {
                        context_lines(&previous[previous.len().saturating_sub(context)..])
                    } else {
                        Vec::new()
                    };
                    old_range_start -= current.len();
                    new_range_start -= current.len();
                }
            }
            for line in lines {
                current.push(format!("{}{line}", if added { '+' } else { '-' }));
            }
            if added {
                new_line += lines.len();
            } else {
                old_line += lines.len();
            }
        } else {
            if old_range_start != 0 {
                if lines.len() <= context * 2 && index + 2 < entries.len() {
                    current.extend(context_lines(lines));
                } else {
                    let context_size = lines.len().min(context);
                    current.extend(context_lines(&lines[..context_size]));
                    hunks.push(Hunk {
                        old_start: old_range_start,
                        old_lines: old_line - old_range_start + context_size,
                        new_start: new_range_start,
                        new_lines: new_line - new_range_start + context_size,
                        lines: std::mem::take(&mut current),
                    });
                    old_range_start = 0;
                    new_range_start = 0;
                }
            }
            old_line += lines.len();
            new_line += lines.len();
        }
    }

    let mut out = vec![format!("--- {old_name}"), format!("+++ {new_name}")];
    for hunk in &mut hunks {
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines.drain(..) {
            match line.strip_suffix('\n') {
                Some(stripped) => lines.push(stripped.to_owned()),
                None => {
                    lines.push(line);
                    lines.push("\\ No newline at end of file".to_owned());
                }
            }
        }
        // Unified diff: an empty range starts one line earlier.
        let old_start = if hunk.old_lines == 0 {
            hunk.old_start - 1
        } else {
            hunk.old_start
        };
        let new_start = if hunk.new_lines == 0 {
            hunk.new_start - 1
        } else {
            hunk.new_start
        };
        out.push(format!(
            "@@ -{old_start},{} +{new_start},{} @@",
            hunk.old_lines, hunk.new_lines
        ));
        out.extend(lines);
    }
    format!("{}\n", out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(changes: &[Change]) -> Vec<(char, &str)> {
        changes
            .iter()
            .map(|change| {
                let kind = if change.added {
                    '+'
                } else if change.removed {
                    '-'
                } else {
                    ' '
                };
                (kind, change.value.as_str())
            })
            .collect()
    }

    // Results recorded from jsdiff 8.0.4's `diffLines`.
    #[test]
    fn matches_jsdiff() {
        assert_eq!(
            parts(&diff_lines("a\nb\nc\n", "a\nx\nc\n")),
            [(' ', "a\n"), ('-', "b\n"), ('+', "x\n"), (' ', "c\n")]
        );
        assert_eq!(parts(&diff_lines("", "a")), [('+', "a")]);
        assert_eq!(parts(&diff_lines("a", "")), [('-', "a")]);
        assert_eq!(parts(&diff_lines("same", "same")), [(' ', "same")]);
        assert_eq!(parts(&diff_lines("", "")), Vec::<(char, &str)>::new());
        assert_eq!(
            parts(&diff_lines("a\r\nb\n", "a\nb\n")),
            [('-', "a\r\n"), ('+', "a\n"), (' ', "b\n")]
        );
    }

    #[test]
    fn long_chains_drop_without_overflow() {
        let old: String = (0..200_000).map(|i| format!("{i}\n")).collect();
        let changes = diff_lines(&old, &format!("{old}x\n"));
        assert_eq!(changes.len(), 2);
    }
}
