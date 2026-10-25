//! Pi's byte- and line-limited tool output, from `src/core/tools/truncate.ts`.
//!
//! Line counting ignores a final newline. Byte limits count UTF-8, while
//! [`truncate_line`] counts UTF-16 code units as Pi's JavaScript does.

use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TruncationOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    /// The only kept line is the UTF-8-safe end of an overlong last line.
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

fn lines(content: &str) -> impl DoubleEndedIterator<Item = &str> {
    // `str::lines` also removes CR in CRLF, which Pi preserves here.
    content.split_terminator('\n')
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else {
        let (unit, suffix) = if bytes < 1024 * 1024 {
            (1024u128, "KB")
        } else {
            (1024 * 1024, "MB")
        };
        // JavaScript's toFixed rounds positive ties upward, not to even.
        let tenths = (bytes as u128 * 10 + unit / 2) / unit;
        format!(
            "{}.{suffix_digit}{suffix}",
            tenths / 10,
            suffix_digit = tenths % 10
        )
    }
}

fn initial(content: &str, options: TruncationOptions) -> TruncationResult {
    let total_lines = lines(content).count();
    let truncated = total_lines > options.max_lines || content.len() > options.max_bytes;
    TruncationResult {
        content: String::new(),
        truncated,
        truncated_by: truncated.then_some(TruncatedBy::Lines),
        total_lines,
        total_bytes: content.len(),
        output_lines: 0,
        output_bytes: 0,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: options.max_lines,
        max_bytes: options.max_bytes,
    }
}

fn unchanged(mut result: TruncationResult, content: &str) -> TruncationResult {
    result.content = content.to_owned();
    result.output_bytes = result.total_bytes;
    result.output_lines = result.total_lines;
    result
}

/// Keep complete lines from the beginning. An overlong first line returns
/// empty content with `first_line_exceeds_limit`, even with a zero line limit.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let mut result = initial(content, options);
    if !result.truncated {
        return unchanged(result, content);
    }
    if lines(content)
        .next()
        .is_some_and(|line| line.len() > options.max_bytes)
    {
        result.truncated_by = Some(TruncatedBy::Bytes);
        result.first_line_exceeds_limit = true;
        return result;
    }
    for line in lines(content).take(options.max_lines) {
        let separator = usize::from(result.output_lines > 0);
        if line.len().saturating_add(separator) > options.max_bytes - result.output_bytes {
            result.truncated_by = Some(TruncatedBy::Bytes);
            break;
        }
        if separator > 0 {
            result.content.push('\n');
        }
        result.content.push_str(line);
        result.output_lines += 1;
        result.output_bytes = result.content.len();
    }
    result
}

/// Keep complete lines from the end. If the last line alone exceeds the
/// byte limit, keep its UTF-8-safe suffix and set `last_line_partial`.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let mut result = initial(content, options);
    if !result.truncated {
        return unchanged(result, content);
    }
    let mut kept = Vec::new();
    for line in lines(content).rev().take(options.max_lines) {
        let separator = usize::from(!kept.is_empty());
        if line.len().saturating_add(separator) > options.max_bytes - result.output_bytes {
            result.truncated_by = Some(TruncatedBy::Bytes);
            if kept.is_empty() {
                let mut start = line.len().saturating_sub(options.max_bytes);
                while !line.is_char_boundary(start) {
                    start += 1;
                }
                kept.push(&line[start..]);
                result.output_bytes = line.len() - start;
                result.last_line_partial = true;
            }
            break;
        }
        kept.push(line);
        result.output_bytes += separator + line.len();
    }
    // Pi gives the line limit precedence even when a partial line hit bytes.
    if kept.len() >= options.max_lines && result.output_bytes <= options.max_bytes {
        result.truncated_by = Some(TruncatedBy::Lines);
    }
    result.output_lines = kept.len();
    kept.reverse();
    result.content = kept.join("\n");
    result
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineTruncationResult {
    pub text: String,
    pub was_truncated: bool,
}

/// Pi's grep-line shortening. If the UTF-16 cut splits a surrogate pair,
/// Rust represents that lone surrogate as U+FFFD.
pub fn truncate_line(line: &str, max_chars: usize) -> LineTruncationResult {
    if line.encode_utf16().count() <= max_chars {
        return LineTruncationResult {
            text: line.to_owned(),
            was_truncated: false,
        };
    }
    let units: Vec<_> = line.encode_utf16().take(max_chars).collect();
    LineTruncationResult {
        text: format!("{}... [truncated]", String::from_utf16_lossy(&units)),
        was_truncated: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiddleTruncationResult {
    pub content: String,
    pub truncated: bool,
    /// Unicode scalar values omitted, as Pi's `Array.from(text).length` counts.
    pub removed_chars: usize,
    pub total_bytes: usize,
    pub total_lines: usize,
}

/// Keep half the byte allowance at each end, with Pi's omission marker.
/// The marker is additional to `max_bytes`; cuts never split UTF-8 characters.
pub fn truncate_middle(content: &str, max_bytes: usize) -> MiddleTruncationResult {
    let mut result = MiddleTruncationResult {
        content: content.to_owned(),
        truncated: false,
        removed_chars: 0,
        total_bytes: content.len(),
        total_lines: lines(content).count(),
    };
    if content.len() <= max_bytes {
        return result;
    }
    let mut head_end = max_bytes / 2;
    while !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = content.len() - (max_bytes - max_bytes / 2);
    while !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    result.truncated = true;
    result.removed_chars = content[head_end..tail_start].chars().count();
    result.content = format!(
        "{}…{} chars truncated…{}",
        &content[..head_end],
        result.removed_chars,
        &content[tail_start..]
    );
    result
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Tool output: short and long lines of ASCII, multibyte, and
    /// astral-plane text, blank lines, and CR before LF, with or without a
    /// final newline.
    fn output() -> impl Strategy<Value = String> {
        let line = prop_oneof![
            "[a-z \t\r]{0,6}",
            "[a-z\u{e9}\u{4e2d}\u{1F600}]{0,12}",
            "x{40,90}",
        ];
        (prop::collection::vec(line, 0..12), any::<bool>()).prop_map(|(lines, newline)| {
            let mut text = lines.join("\n");
            if newline {
                text.push('\n');
            }
            text
        })
    }

    fn options() -> impl Strategy<Value = TruncationOptions> {
        (0..12usize, 0..120usize).prop_map(|(max_lines, max_bytes)| TruncationOptions {
            max_lines,
            max_bytes,
        })
    }

    /// The facts every result reports about its input.
    fn check_totals(
        result: &TruncationResult,
        content: &str,
        options: TruncationOptions,
    ) -> Result<(), TestCaseError> {
        let total_lines = lines(content).count();
        prop_assert_eq!(result.total_lines, total_lines);
        prop_assert_eq!(result.total_bytes, content.len());
        prop_assert_eq!(
            result.truncated,
            total_lines > options.max_lines || content.len() > options.max_bytes
        );
        prop_assert_eq!(result.truncated_by.is_some(), result.truncated);
        prop_assert_eq!(result.output_bytes, result.content.len());
        if !result.truncated {
            prop_assert_eq!(&result.content, content);
            prop_assert_eq!(result.output_lines, total_lines);
        } else {
            prop_assert!(result.content.len() <= options.max_bytes);
            prop_assert!(result.output_lines <= options.max_lines);
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn head_truncation_keeps_the_most_whole_lines_that_fit_from_the_start(
            content in output(),
            options in options(),
        ) {
            let result = truncate_head(&content, options);
            check_totals(&result, &content, options)?;
            if !result.truncated {
                return Ok(());
            }
            let all: Vec<&str> = lines(&content).collect();
            let first_too_long = all.first().is_some_and(|line| line.len() > options.max_bytes);
            prop_assert_eq!(result.first_line_exceeds_limit, first_too_long);
            prop_assert!(!result.last_line_partial);
            if first_too_long {
                prop_assert_eq!(result.content.as_str(), "");
                prop_assert_eq!(result.output_lines, 0);
                prop_assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
                return Ok(());
            }
            prop_assert!(result.output_lines <= options.max_lines);
            prop_assert_eq!(&result.content, &all[..result.output_lines].join("\n"));
            let room = options.max_lines.min(all.len());
            if result.output_lines < room {
                // The next line did not fit in the bytes left.
                let next = all[result.output_lines];
                let separator = usize::from(result.output_lines > 0);
                prop_assert!(result.output_bytes + separator + next.len() > options.max_bytes);
                prop_assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
            }
        }

        #[test]
        fn tail_truncation_keeps_the_most_whole_lines_that_fit_from_the_end(
            content in output(),
            options in options(),
        ) {
            let result = truncate_tail(&content, options);
            check_totals(&result, &content, options)?;
            prop_assert!(!result.first_line_exceeds_limit);
            if !result.truncated {
                return Ok(());
            }
            let all: Vec<&str> = lines(&content).collect();
            prop_assert!(result.output_lines <= options.max_lines);
            if result.last_line_partial {
                // The last line alone was too long: its shortest suffix
                // that fits, cut between characters.
                let last = all.last().copied().unwrap_or_default();
                prop_assert!(last.len() > options.max_bytes);
                prop_assert_eq!(result.output_lines, 1);
                prop_assert!(last.ends_with(result.content.as_str()));
                let cut = last.len() - result.content.len();
                let shortest = last[..cut]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| result.content.len() + ch.len_utf8() > options.max_bytes);
                prop_assert!(shortest);
                return Ok(());
            }
            let kept = &all[all.len() - result.output_lines..];
            prop_assert_eq!(&result.content, &kept.join("\n"));
            let room = options.max_lines.min(all.len());
            if result.output_lines < room {
                let next = all[all.len() - result.output_lines - 1];
                let separator = usize::from(result.output_lines > 0);
                prop_assert!(result.output_bytes + separator + next.len() > options.max_bytes);
                prop_assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
            }
        }

        #[test]
        fn middle_truncation_keeps_a_head_and_tail_within_half_the_bytes_each(
            content in output(),
            max_bytes in 0..120usize,
        ) {
            let result = truncate_middle(&content, max_bytes);
            prop_assert_eq!(result.total_bytes, content.len());
            prop_assert_eq!(result.total_lines, lines(&content).count());
            prop_assert_eq!(result.truncated, content.len() > max_bytes);
            if !result.truncated {
                prop_assert_eq!(&result.content, &content);
                prop_assert_eq!(result.removed_chars, 0);
                return Ok(());
            }
            let marker = format!("…{} chars truncated…", result.removed_chars);
            let (head_room, tail_room) = (max_bytes / 2, max_bytes - max_bytes / 2);
            // Some head and tail of the content, around the marker.
            let found = (0..=head_room.min(content.len()))
                .filter(|&head| content.is_char_boundary(head))
                .find_map(|head| {
                    let tail = result.content.len().checked_sub(head + marker.len())?;
                    let tail_start = content.len().checked_sub(tail)?;
                    let rebuilt = format!("{}{marker}{}", &content[..head], content.get(tail_start..)?);
                    (rebuilt == result.content).then_some((head, tail_start))
                });
            let Some((head, tail_start)) = found else {
                return Err(TestCaseError::fail(format!("{:?} is not a head and tail of {content:?}", result.content)));
            };
            prop_assert!(content.len() - tail_start <= tail_room);
            prop_assert!(head <= tail_start);
            prop_assert_eq!(result.removed_chars, content[head..tail_start].chars().count());
            // Neither end could keep another character.
            let head_full = content[head..]
                .chars()
                .next()
                .is_none_or(|ch| head + ch.len_utf8() > head_room);
            let tail_full = content[..tail_start]
                .chars()
                .next_back()
                .is_none_or(|ch| content.len() - tail_start + ch.len_utf8() > tail_room);
            prop_assert!(head_full && tail_full);
        }

        #[test]
        fn line_truncation_keeps_exactly_the_utf16_limit_and_marks_the_cut(
            line in "[a-z\u{e9}\u{4e2d}\u{1F600}]{0,20}",
            max_chars in 0..24usize,
        ) {
            let result = truncate_line(&line, max_chars);
            let units = line.encode_utf16().count();
            prop_assert_eq!(result.was_truncated, units > max_chars);
            if !result.was_truncated {
                prop_assert_eq!(&result.text, &line);
                return Ok(());
            }
            let kept = result.text.strip_suffix("... [truncated]");
            prop_assert!(kept.is_some(), "{:?} has no marker", result.text);
            let kept = kept.unwrap_or_default();
            // A cut through a surrogate pair leaves U+FFFD, one unit too.
            prop_assert_eq!(kept.encode_utf16().count(), max_chars);
            let whole = kept.strip_suffix('\u{FFFD}').unwrap_or(kept);
            prop_assert!(line.starts_with(whole));
        }
    }
}
