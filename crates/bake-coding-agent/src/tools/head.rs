//! [`truncate_head`] over lines that arrive one at a time, in bounded
//! memory: the result equals `truncate_head(&lines.join("\n"), options)`.
//!
//! Only the lines the head can keep, and one past them, are held; the rest
//! count toward the totals. The read and grep tools use this so that a file
//! or output of any size costs no more memory than its truncated head.

use crate::tools::truncate::{TruncationOptions, TruncationResult, truncate_head};

#[derive(Debug)]
pub(crate) struct HeadCollector {
    options: TruncationOptions,
    held: Vec<String>,
    held_bytes: usize,
    holding: bool,
    count: usize,
    total_bytes: usize,
    last_empty: bool,
}

impl HeadCollector {
    pub(crate) fn new(options: TruncationOptions) -> Self {
        Self {
            options,
            held: Vec::new(),
            held_bytes: 0,
            holding: true,
            count: 0,
            total_bytes: 0,
            last_empty: false,
        }
    }

    /// Whether the next line's text can reach the result. When not, pass
    /// only its length to [`Self::push_len`].
    pub(crate) fn wants_text(&self) -> bool {
        self.holding
    }

    /// The longest line text [`Self::push`] needs; longer lines may be
    /// passed to [`Self::push_len`] instead, since they cannot be kept.
    pub(crate) fn max_line_bytes(&self) -> usize {
        self.options.max_bytes
    }

    pub(crate) fn push(&mut self, line: &str) {
        self.record(line.len());
        if self.holding {
            self.hold(line.len(), || line.to_owned());
        }
    }

    /// A line known only by its UTF-8 length, which must exceed
    /// [`Self::max_line_bytes`] if [`Self::wants_text`] was true.
    pub(crate) fn push_len(&mut self, len: usize) {
        self.record(len);
        if self.holding {
            // Never kept: a longer line ends the head where it starts.
            let stand_in = self.options.max_bytes.saturating_add(1);
            self.hold(len, || "x".repeat(stand_in));
        }
    }

    fn record(&mut self, len: usize) {
        if self.count > 0 {
            self.total_bytes = self.total_bytes.saturating_add(1);
        }
        self.total_bytes = self.total_bytes.saturating_add(len);
        self.count += 1;
        self.last_empty = len == 0;
    }

    fn hold(&mut self, len: usize, text: impl FnOnce() -> String) {
        if !self.held.is_empty() {
            self.held_bytes = self.held_bytes.saturating_add(1);
        }
        self.held_bytes = self.held_bytes.saturating_add(len);
        self.held.push(text());
        // Two lines past the limit: a trailing empty line does not count.
        if self.held.len() >= self.options.max_lines.saturating_add(2)
            || self.held_bytes > self.options.max_bytes
        {
            self.holding = false;
        }
    }

    pub(crate) fn finish(self) -> TruncationResult {
        let total_lines = self.count - usize::from(self.last_empty);
        let mut held = self.held;
        if held.len() < self.count {
            // More lines followed, so a held trailing empty line is a line
            // of the content, not the end of it. The stand-in lies past the
            // point where the head stops.
            held.push("x".to_owned());
        }
        let mut result = truncate_head(&held.join("\n"), self.options);
        result.total_lines = total_lines;
        result.total_bytes = self.total_bytes;
        let truncated =
            total_lines > self.options.max_lines || self.total_bytes > self.options.max_bytes;
        if truncated && !result.truncated {
            // Unreachable by construction; kept so a mistake cannot hide.
            result.truncated_by = Some(crate::tools::truncate::TruncatedBy::Lines);
        }
        result.truncated = truncated;
        if !truncated {
            result.truncated_by = None;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(lines: &[&str], options: TruncationOptions) -> TruncationResult {
        let mut collector = HeadCollector::new(options);
        for line in lines {
            if collector.wants_text() && line.len() <= collector.max_line_bytes() {
                collector.push(line);
            } else {
                collector.push_len(line.len());
            }
        }
        collector.finish()
    }

    #[test]
    fn equals_truncate_head_of_the_joined_lines() {
        let mut seed = 0x2545_f491_u32;
        let mut next = |max: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % max
        };
        for _ in 0..4000 {
            let count = next(12) as usize;
            let lines: Vec<String> = (0..count)
                .map(|_| {
                    let len = next(9) as usize;
                    "é".repeat(len / 2) + &"a".repeat(len % 2)
                })
                .collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let options = TruncationOptions {
                max_lines: next(6) as usize,
                max_bytes: next(20) as usize,
            };
            assert_eq!(
                collect(&refs, options),
                truncate_head(&refs.join("\n"), options),
                "{refs:?} {options:?}"
            );
        }
    }
}
