//! Matching, replacement, and diffs for the edit tool, ported from Pi
//! `src/core/tools/edit-diff.ts` (v1.1.0; see the crate's `NOTICE`).
//!
//! Offsets are UTF-8 byte offsets where Pi's are UTF-16 indices; every
//! offset is a match boundary or line boundary in the same string, so the
//! results are the same.

use crate::tools::jsdiff;

/// Pi's `Edit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Text to find.
    pub old_text: String,
    /// Replacement.
    pub new_text: String,
}

/// A file's line ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`
    Lf,
    /// `\r\n`
    CrLf,
}

/// Pi's `detectLineEnding`: CRLF when the first `\n` follows a `\r`.
pub fn detect_line_ending(content: &str) -> LineEnding {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf < lf => LineEnding::CrLf,
        _ => LineEnding::Lf,
    }
}

/// Pi's `normalizeToLF`.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Pi's `restoreLineEndings`.
pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::CrLf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_owned(),
    }
}

/// Pi's `normalizeForFuzzyMatch`: NFKC, trailing whitespace removed from
/// each line, and typographic quotes, dashes, and spaces made ASCII.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc = icu_normalizer::ComposingNormalizerBorrowed::new_nfkc().normalize(text);
    let trimmed = nfkc
        .split('\n')
        .map(crate::tools::js::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    trimmed
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Pi's `FuzzyMatchResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Where the match starts in [`Self::content_for_replacement`].
    pub index: usize,
    /// The match's length there.
    pub match_length: usize,
    /// Whether the normalized forms matched instead of the exact text.
    pub used_fuzzy_match: bool,
    /// The content the offsets refer to.
    pub content_for_replacement: String,
}

/// Pi's `fuzzyFindText`: an exact match, else one between the fuzzy forms.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> Option<FuzzyMatch> {
    if let Some(index) = content.find(old_text) {
        return Some(FuzzyMatch {
            index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_owned(),
        });
    }
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    let index = fuzzy_content.find(&fuzzy_old)?;
    Some(FuzzyMatch {
        index,
        match_length: fuzzy_old.len(),
        used_fuzzy_match: true,
        content_for_replacement: fuzzy_content,
    })
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    if fuzzy_old.is_empty() {
        // JavaScript's `split("")` yields one piece per UTF-16 unit.
        return fuzzy_content.encode_utf16().count().saturating_sub(1);
    }
    fuzzy_content.matches(&fuzzy_old).count()
}

#[derive(Debug, Clone)]
struct Replacement {
    edit_index: usize,
    index: usize,
    length: usize,
    new_text: String,
}

fn split_lines_with_endings(content: &str) -> Vec<&str> {
    content.split_inclusive('\n').collect()
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = (offset, offset + line.len());
            offset = span.1;
            span
        })
        .collect()
}

fn replacement_line_range(
    lines: &[(usize, usize)],
    replacement: &Replacement,
) -> Result<(usize, usize), String> {
    let start = replacement.index;
    let end = replacement.index + replacement.length;
    let start_line = lines
        .iter()
        .position(|(line_start, line_end)| start >= *line_start && start < *line_end)
        .ok_or("Replacement range is outside the base content.")?;
    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].1 < end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err("Replacement range is outside the base content.".into());
    }
    Ok((start_line, end_line + 1))
}

fn apply_replacements(content: &str, replacements: &[Replacement], offset: usize) -> String {
    let mut result = content.to_owned();
    for replacement in replacements.iter().rev() {
        let index = replacement.index - offset;
        result.replace_range(index..index + replacement.length, &replacement.new_text);
    }
    result
}

/// Pi's `applyReplacementsPreservingUnchangedLines`: replacements matched
/// in a normalized view, written over the original with every line they do
/// not touch kept byte for byte.
fn apply_replacements_preserving_unchanged_lines(
    original: &str,
    base: &str,
    replacements: &[Replacement],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original);
    let base_lines = line_spans(base);
    if original_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .into(),
        );
    }
    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|replacement| replacement.index);
    let mut groups: Vec<(usize, usize, Vec<Replacement>)> = Vec::new();
    for replacement in sorted {
        let (start, end) = replacement_line_range(&base_lines, &replacement)?;
        if let Some(current) = groups.last_mut()
            && start < current.1
        {
            current.1 = current.1.max(end);
            current.2.push(replacement);
            continue;
        }
        groups.push((start, end, vec![replacement]));
    }
    let mut result = String::new();
    let mut original_index = 0;
    for (start, end, group) in &groups {
        result.push_str(&original_lines[original_index..*start].concat());
        let group_start = base_lines[*start].0;
        let group_end = base_lines[end - 1].1;
        result.push_str(&apply_replacements(
            &base[group_start..group_end],
            group,
            group_start,
        ));
        original_index = *end;
    }
    result.push_str(&original_lines[original_index..].concat());
    Ok(result)
}

fn not_found(path: &str, index: usize, total: usize) -> String {
    if total == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

fn duplicate(path: &str, index: usize, total: usize, occurrences: usize) -> String {
    if total == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn empty_old_text(path: &str, index: usize, total: usize) -> String {
    if total == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{index}].oldText must not be empty in {path}.")
    }
}

fn no_change(path: &str, total: usize) -> String {
    if total == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

/// Pi's `AppliedEditsResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEdits {
    /// The content the edits applied to.
    pub base_content: String,
    /// The result.
    pub new_content: String,
}

/// Pi's `applyEditsToNormalizedContent`: every edit matched against the
/// original LF content, exactly or else fuzzily, unique and disjoint, then
/// applied together. Errors carry Pi's messages.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEdits, String> {
    let edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();
    let total = edits.len();
    if let Some(index) = edits.iter().position(|edit| edit.old_text.is_empty()) {
        return Err(empty_old_text(path, index, total));
    }
    let used_fuzzy = edits.iter().any(|edit| {
        fuzzy_find_text(normalized_content, &edit.old_text)
            .is_some_and(|found| found.used_fuzzy_match)
    });
    let base = if used_fuzzy {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_owned()
    };
    let mut matched: Vec<Replacement> = Vec::with_capacity(total);
    for (index, edit) in edits.iter().enumerate() {
        let Some(found) = fuzzy_find_text(&base, &edit.old_text) else {
            return Err(not_found(path, index, total));
        };
        let occurrences = count_occurrences(&base, &edit.old_text);
        if occurrences > 1 {
            return Err(duplicate(path, index, total, occurrences));
        }
        matched.push(Replacement {
            edit_index: index,
            index: found.index,
            length: found.match_length,
            new_text: edit.new_text.clone(),
        });
    }
    // Stable, as JavaScript's sort is.
    matched.sort_by_key(|replacement| replacement.index);
    for pair in matched.windows(2) {
        if pair[0].index + pair[0].length > pair[1].index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                pair[0].edit_index, pair[1].edit_index
            ));
        }
    }
    let new_content = if used_fuzzy {
        apply_replacements_preserving_unchanged_lines(normalized_content, &base, &matched)?
    } else {
        apply_replacements(&base, &matched, 0)
    };
    if normalized_content == new_content {
        return Err(no_change(path, total));
    }
    Ok(AppliedEdits {
        base_content: normalized_content.to_owned(),
        new_content,
    })
}

/// Pi's `generateUnifiedPatch` with four lines of context.
pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str) -> String {
    jsdiff::create_two_files_patch(path, path, old_content, new_content, 4)
}

/// Pi's `generateDiffString` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffString {
    /// The numbered diff.
    pub diff: String,
    /// The first changed line in the new file.
    pub first_changed_line: Option<usize>,
}

/// Pi's `generateDiffString` with four lines of context: numbered `+`,
/// `-`, and context lines, with `...` for skipped runs.
pub fn generate_diff_string(old_content: &str, new_content: &str) -> DiffString {
    const CONTEXT: usize = 4;
    let parts = jsdiff::diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();
    let max_line = old_content
        .split('\n')
        .count()
        .max(new_content.split('\n').count());
    let width = max_line.to_string().len();
    let number = |n: usize| format!("{n:>width$}");
    let blank = " ".repeat(width);
    let (mut old_line, mut new_line) = (1usize, 1usize);
    let mut last_was_change = false;
    let mut first_changed_line = None;
    for (index, part) in parts.iter().enumerate() {
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }
        if part.added || part.removed {
            first_changed_line.get_or_insert(new_line);
            for line in raw {
                if part.added {
                    output.push(format!("+{} {line}", number(new_line)));
                    new_line += 1;
                } else {
                    output.push(format!("-{} {line}", number(old_line)));
                    old_line += 1;
                }
            }
            last_was_change = true;
            continue;
        }
        let next_is_change = parts
            .get(index + 1)
            .is_some_and(|next| next.added || next.removed);
        let context = |output: &mut Vec<String>,
                       lines: &[&str],
                       old_line: &mut usize,
                       new_line: &mut usize| {
            for line in lines {
                output.push(format!(" {} {line}", number(*old_line)));
                *old_line += 1;
                *new_line += 1;
            }
        };
        if last_was_change && next_is_change {
            if raw.len() <= CONTEXT * 2 {
                context(&mut output, &raw, &mut old_line, &mut new_line);
            } else {
                let skipped = raw.len() - 2 * CONTEXT;
                context(&mut output, &raw[..CONTEXT], &mut old_line, &mut new_line);
                output.push(format!(" {blank} ..."));
                old_line += skipped;
                new_line += skipped;
                context(
                    &mut output,
                    &raw[raw.len() - CONTEXT..],
                    &mut old_line,
                    &mut new_line,
                );
            }
        } else if last_was_change {
            let shown = raw.len().min(CONTEXT);
            context(&mut output, &raw[..shown], &mut old_line, &mut new_line);
            let skipped = raw.len() - shown;
            if skipped > 0 {
                output.push(format!(" {blank} ..."));
                old_line += skipped;
                new_line += skipped;
            }
        } else if next_is_change {
            let skipped = raw.len().saturating_sub(CONTEXT);
            if skipped > 0 {
                output.push(format!(" {blank} ..."));
                old_line += skipped;
                new_line += skipped;
            }
            context(&mut output, &raw[skipped..], &mut old_line, &mut new_line);
        } else {
            old_line += raw.len();
            new_line += raw.len();
        }
        last_was_change = false;
    }
    DiffString {
        diff: output.join("\n"),
        first_changed_line,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Line text with the characters fuzzy matching rewrites: typographic
    /// quotes, dashes, and spaces, a ligature and a fullwidth letter that
    /// NFKC folds, and tabs and spaces that trail a line.
    fn line_text() -> impl Strategy<Value = String> {
        "[a-z \t\u{2019}\u{201C}\u{2014}\u{a0}\u{fb01}\u{ff21}]{0,10}"
    }

    /// Replacement text, already LF-normalized as the edit tool passes it.
    fn new_text() -> impl Strategy<Value = String> {
        "[a-z \n\u{e9}\u{1F600}]{0,10}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn unique_exact_edits_replace_exactly_their_matches_in_any_order(
            fillers in prop::collection::vec("[a-z \n\u{4e2d}]{0,8}", 4),
            targets in prop::collection::vec(("[a-z\n]{0,4}", new_text()), 1..4),
            rotation in any::<usize>(),
        ) {
            // `<k|…|>` marks make each old text unique.
            let mut content = fillers[0].clone();
            let mut expected = content.clone();
            let mut edits = Vec::new();
            for (k, (body, replacement)) in targets.iter().enumerate() {
                let old_text = format!("<{k}|{body}|>");
                content.push_str(&old_text);
                content.push_str(&fillers[k + 1]);
                expected.push_str(replacement);
                expected.push_str(&fillers[k + 1]);
                edits.push(Edit { old_text, new_text: replacement.clone() });
            }
            let len = edits.len();
            edits.rotate_left(rotation % len);
            let applied = apply_edits_to_normalized_content(&content, &edits, "f");
            if expected == content {
                prop_assert!(applied.is_err());
            } else {
                let applied = applied.map_err(TestCaseError::fail)?;
                prop_assert_eq!(applied.base_content, content);
                prop_assert_eq!(applied.new_content, expected);
            }
        }

        #[test]
        fn an_old_text_found_twice_is_refused(
            before in "[a-z \n]{0,6}",
            between in "[a-z \n]{0,6}",
            after in "[a-z \n]{0,6}",
            old_text in "[a-z\u{2019}\n]{1,6}",
            replacement in new_text(),
        ) {
            let content = format!("{before}{old_text}{between}{old_text}{after}");
            let edit = Edit { old_text, new_text: replacement };
            let error = apply_edits_to_normalized_content(&content, &[edit], "f");
            prop_assert!(
                error.as_ref().is_err_and(|message| message.contains("occurrences")),
                "{error:?}"
            );
        }

        #[test]
        fn a_fuzzy_edit_rewrites_only_the_lines_it_matches(
            texts in prop::collection::vec(line_text(), 1..8),
            range in (0..8usize, 1..4usize),
            replacement in new_text(),
        ) {
            // `<k>` opens each line, so the matched lines occur once.
            let lines: Vec<String> = texts
                .iter()
                .enumerate()
                .map(|(k, text)| format!("<{k}>{text}"))
                .collect();
            let start = range.0 % lines.len();
            let end = (start + range.1).min(lines.len());
            let content = lines.join("\n");
            let old_text = normalize_for_fuzzy_match(&lines[start..end].join("\n"));
            let edit = Edit { old_text, new_text: replacement.clone() };
            let before: String = lines[..start].iter().map(|line| format!("{line}\n")).collect();
            let after: String = lines[end..].iter().map(|line| format!("\n{line}")).collect();
            match apply_edits_to_normalized_content(&content, &[edit], "f") {
                Ok(applied) => {
                    // Untouched lines are kept byte for byte; only blanks an
                    // exact match left at the end of its last line remain.
                    let edited = applied
                        .new_content
                        .strip_prefix(before.as_str())
                        .and_then(|rest| rest.strip_suffix(after.as_str()))
                        .and_then(|rest| rest.strip_prefix(replacement.as_str()));
                    prop_assert!(
                        edited.is_some_and(|rest| rest.trim().is_empty()),
                        "{:?} does not keep {before:?} and {after:?} around {replacement:?}",
                        applied.new_content
                    );
                }
                Err(message) => prop_assert!(message.starts_with("No changes"), "{message}"),
            }
        }
    }
}
