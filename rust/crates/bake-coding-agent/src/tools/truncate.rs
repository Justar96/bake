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
