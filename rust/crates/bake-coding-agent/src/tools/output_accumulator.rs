//! Pi's `src/core/tools/output-accumulator.ts`: a decoded display tail and
//! a raw output file once either display limit is exceeded.
//!
//! I/O is synchronous and backpressures the caller; there are no background
//! writes. [`OutputAccumulator::finish`] flushes the decoder and closes the
//! file. An I/O failure poisons the accumulator so incomplete output cannot
//! be reported as complete. Spill files survive drop for subsequent tool
//! reads; their owner must remove the private directory when no longer needed.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use super::output_files;
use super::truncate::{TruncatedBy, TruncationOptions, TruncationResult, truncate_tail};
use super::utf8::{Decoder, decode_all};

#[derive(Debug, Clone)]
pub struct OutputAccumulatorOptions {
    pub limits: TruncationOptions,
    /// Existing, trusted parent directory. Defaults to the OS temporary directory.
    /// Unix spills use a new 0700 directory and 0600 file; Windows inherits ACLs.
    pub temp_directory: PathBuf,
    /// ASCII letters, digits, hyphens and underscores only.
    pub temp_file_prefix: String,
}

impl Default for OutputAccumulatorOptions {
    fn default() -> Self {
        Self {
            limits: TruncationOptions::default(),
            temp_directory: std::env::temp_dir(),
            temp_file_prefix: "pi-output".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullOutput {
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    Open,
    Finished,
    Failed,
}

/// Retains at most four times the display byte limit as decoded tail text
/// (or four bytes for a zero limit), plus at most the byte limit as raw data.
/// An individual append can temporarily allocate in proportion to its input.
#[derive(Debug)]
pub struct OutputAccumulator {
    options: OutputAccumulatorOptions,
    max_rolling_bytes: usize,
    decoder: Decoder,
    raw: Vec<u8>,
    tail: String,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    state: State,
    path: Option<PathBuf>,
    file: Option<File>,
}

impl OutputAccumulator {
    /// Creates an empty accumulator without filesystem effects. Invalid prefixes
    /// return `InvalidInput`; zero limits are supported as Pi supports them.
    pub fn new(options: OutputAccumulatorOptions) -> io::Result<Self> {
        output_files::validate_prefix(&options.temp_file_prefix)?;
        Ok(Self {
            max_rolling_bytes: options.limits.max_bytes.saturating_mul(2).max(1),
            options,
            decoder: Decoder::default(),
            raw: Vec::new(),
            tail: String::new(),
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            state: State::Open,
            path: None,
            file: None,
        })
    }

    /// Appends one process-output chunk. Invalid UTF-8 becomes U+FFFD in
    /// snapshots; the spill file always preserves the original bytes.
    /// After finish or an I/O failure, returns `InvalidInput` without writing.
    pub fn append(&mut self, data: &[u8]) -> io::Result<()> {
        if self.state != State::Open {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot append to a finished or failed output accumulator",
            ));
        }
        let result = self.append_inner(data);
        self.record_failure(result)
    }

    fn append_inner(&mut self, data: &[u8]) -> io::Result<()> {
        self.total_raw_bytes = self
            .total_raw_bytes
            .checked_add(data.len())
            .ok_or_else(counter_overflow)?;
        let text = self.decoder.decode(data);
        self.append_decoded(&text)?;
        if self.should_spill() {
            self.ensure_file()?;
        }
        match &mut self.file {
            Some(file) => file.write_all(data),
            None => {
                self.raw.extend_from_slice(data);
                Ok(())
            }
        }
    }

    /// Flushes an unfinished UTF-8 sequence, persists any newly truncated
    /// output, and closes the file. Repeated successful calls are harmless.
    /// Files are not fsynced; this is output retention, not a durable journal.
    pub fn finish(&mut self) -> io::Result<()> {
        self.check_healthy()?;
        if self.state == State::Finished {
            return Ok(());
        }
        let result = (|| {
            let rest = self.decoder.finish();
            self.append_decoded(rest)?;
            if self.should_spill() {
                self.ensure_file()?;
            }
            if let Some(mut file) = self.file.take() {
                file.flush()?;
            }
            self.state = State::Finished;
            Ok(())
        })();
        self.record_failure(result)
    }

    /// A display snapshot without finalizing the decoder. Truncated output
    /// already has a spill file; no separate persistence request is needed.
    pub fn snapshot(&self) -> io::Result<OutputSnapshot> {
        self.check_healthy()?;
        let text = if self.tail_starts_at_line_boundary {
            self.tail.as_str()
        } else {
            self.tail
                .find('\n')
                .map_or(self.tail.as_str(), |index| &self.tail[index + 1..])
        };
        let mut truncation = truncate_tail(text, self.options.limits);
        truncation.truncated = self.total_lines > self.options.limits.max_lines
            || self.total_decoded_bytes > self.options.limits.max_bytes;
        truncation.truncated_by = if truncation.truncated {
            truncation.truncated_by.or(Some(
                if self.total_decoded_bytes > self.options.limits.max_bytes {
                    TruncatedBy::Bytes
                } else {
                    TruncatedBy::Lines
                },
            ))
        } else {
            None
        };
        truncation.total_lines = self.total_lines;
        truncation.total_bytes = self.total_decoded_bytes;
        Ok(OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.path.clone(),
        })
    }

    /// Decoded UTF-8 bytes since the last newline, excluding pending bytes.
    pub fn last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    /// Read after [`Self::finish`]. A spilled file exceeding `max_bytes`
    /// keeps its first and last halves around Pi's byte-omission marker.
    /// As in Pi, output still held in memory ignores this read limit. The
    /// marker is additional to it; incomplete boundary characters are omitted.
    pub fn read_full_output(&self, max_bytes: usize) -> io::Result<FullOutput> {
        self.check_healthy()?;
        if self.state != State::Finished {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Finish the output accumulator before reading full output",
            ));
        }
        let Some(path) = &self.path else {
            return Ok(FullOutput {
                content: decode_all(&self.raw),
                truncated: false,
            });
        };
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        if size <= max_bytes as u64 {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            return Ok(FullOutput {
                content: decode_all(&bytes),
                truncated: false,
            });
        }
        let head_bytes = max_bytes / 2;
        let tail_bytes = max_bytes - head_bytes;
        let mut head = vec![0; head_bytes];
        let mut tail = vec![0; tail_bytes];
        file.read_exact(&mut head)?;
        file.seek(SeekFrom::Start(size - tail_bytes as u64))?;
        file.read_exact(&mut tail)?;
        let head_text = Decoder::default().decode(&head);
        let tail_start = tail
            .iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(tail.len());
        let tail_text = decode_all(&tail[tail_start..]);
        let omitted = size - max_bytes as u64;
        Ok(FullOutput {
            content: format!("{head_text}\n\n[... {omitted} bytes omitted ...]\n\n{tail_text}"),
            truncated: true,
        })
    }

    fn append_decoded(&mut self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.total_decoded_bytes = self
            .total_decoded_bytes
            .checked_add(text.len())
            .ok_or_else(counter_overflow)?;
        self.tail.push_str(text);
        if self.tail.len() > self.max_rolling_bytes.saturating_mul(2) {
            let mut start = self.tail.len() - self.max_rolling_bytes;
            while !self.tail.is_char_boundary(start) {
                start += 1;
            }
            self.tail_starts_at_line_boundary = self.tail.as_bytes()[start - 1] == b'\n';
            // Reallocate only the retained suffix; a huge chunk must not leave
            // its allocation resident for the rest of the process lifetime.
            self.tail = self.tail[start..].to_owned();
        }
        let newlines = text.bytes().filter(|byte| *byte == b'\n').count();
        if let Some(last_newline) = text.rfind('\n') {
            self.completed_lines = self
                .completed_lines
                .checked_add(newlines)
                .ok_or_else(counter_overflow)?;
            self.current_line_bytes = text.len() - last_newline - 1;
        } else {
            self.current_line_bytes = self
                .current_line_bytes
                .checked_add(text.len())
                .ok_or_else(counter_overflow)?;
        }
        self.total_lines = self
            .completed_lines
            .checked_add(usize::from(self.current_line_bytes > 0))
            .ok_or_else(counter_overflow)?;
        Ok(())
    }

    fn should_spill(&self) -> bool {
        self.total_raw_bytes > self.options.limits.max_bytes
            || self.total_decoded_bytes > self.options.limits.max_bytes
            || self.total_lines > self.options.limits.max_lines
    }

    fn ensure_file(&mut self) -> io::Result<()> {
        if self.path.is_some() {
            return Ok(());
        }
        let (path, mut file) =
            output_files::create(&self.options.temp_directory, &self.options.temp_file_prefix)?;
        if let Err(error) = file.write_all(&self.raw) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            return Err(error);
        }
        self.raw = Vec::new();
        self.path = Some(path);
        self.file = Some(file);
        Ok(())
    }

    fn check_healthy(&self) -> io::Result<()> {
        if self.state == State::Failed {
            Err(io::Error::other(
                "Output accumulator failed; complete output is unavailable",
            ))
        } else {
            Ok(())
        }
    }

    fn record_failure(&mut self, result: io::Result<()>) -> io::Result<()> {
        if result.is_err() {
            self.state = State::Failed;
            self.file = None;
        }
        result
    }
}

fn counter_overflow() -> io::Error {
    io::Error::other("Output byte or line count exceeds the platform limit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_chunks_release_their_allocation_after_trimming() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions {
            limits: TruncationOptions {
                max_lines: 2000,
                max_bytes: 1024,
            },
            ..Default::default()
        })
        .unwrap();
        // This pure stage owns tail allocation; no file or global temp path is used.
        accumulator
            .append_decoded(&"x".repeat(8 * 1024 * 1024))
            .unwrap();
        assert_eq!(accumulator.tail.len(), 2048);
        assert_eq!(accumulator.tail.capacity(), 2048);
        assert_eq!(accumulator.snapshot().unwrap().content, "x".repeat(1024));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_write_failure_closes_the_file_and_poisons_the_accumulator() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default()).unwrap();
        // The kernel's failing device exercises a real write error, including
        // the error after a spill has already opened successfully.
        accumulator.file = Some(File::options().write(true).open("/dev/full").unwrap());
        assert!(accumulator.append(b"cannot write").is_err());
        assert!(accumulator.file.is_none());
        assert_eq!(accumulator.state, State::Failed);
        assert!(accumulator.snapshot().is_err());
        assert!(accumulator.finish().is_err());
    }
}
