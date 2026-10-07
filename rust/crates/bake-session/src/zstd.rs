//! Current Zstd log framing and plaintext recovery. Structural validation
//! precedes decoding; complete frames are decoded before their plaintext is
//! scanned. A torn-frame decoding error discards all output from that frame.

use zstd_safe::{DCtx, InBuffer, OutBuffer};

use crate::scan::LogScanner;
use crate::{PathPlatform, RestoreRefusal, RestoredLog, StagedLog, TornTail};

/// Production Zstd framing or decoding refusals. Offsets are physical bytes
/// in the compressed input; plaintext scan refusals remain separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZstdRefusal {
    Empty,
    Magic { offset: usize },
    ReservedHeaderBit { offset: usize },
    ReservedBlockType { offset: usize },
    Frame { start: usize },
    HeaderFrame,
    CompleteFramesUncommitted,
}

impl ZstdRefusal {
    /// The production reader's message, without its file-path wrapper.
    pub fn message(&self) -> String {
        let detail = match self {
            Self::Empty => return "empty or header-less Zstandard session log".into(),
            Self::Magic { offset } => format!("invalid frame magic at byte {offset}"),
            Self::ReservedHeaderBit { offset } => {
                format!("reserved frame-header bit at byte {offset}")
            }
            Self::ReservedBlockType { offset } => format!("reserved block type at byte {offset}"),
            Self::Frame { start } => format!("frame at byte {start} failed validation"),
            Self::HeaderFrame => "first frame is not exactly one header line".into(),
            Self::CompleteFramesUncommitted => "complete frame contains a torn JSONL record".into(),
        };
        format!("corrupt Zstandard session log: {detail}")
    }
}

/// Restore an in-memory current-format Zstd log, including complete rows
/// recoverable from its torn final frame. No file is read, truncated or written.
///
/// `max_plaintext_bytes` bounds cumulative decoder output, including the header
/// and torn frame, before it is retained or scanned. It has no implicit default;
/// exceeding it is a native-only refusal. The decoder retains its normal window
/// limit. `source_budget` separately bounds each row's expanded source references.
pub fn restore_zstd_log(
    log: &[u8],
    platform: PathPlatform,
    source_budget: usize,
    max_plaintext_bytes: usize,
) -> Result<RestoredLog, RestoreRefusal> {
    stage_zstd_log(log, platform, source_budget, max_plaintext_bytes)?.restore()
}

/// Decode and scan an in-memory current-format Zstd log, keeping its physical
/// torn-tail metadata, without the later `validateStoredEvents` passes or
/// restoration; see [`StagedLog`].
/// Refuses only with `Zstd`, `Scan`, or `NativePlaintextBudget`.
pub fn stage_zstd_log(
    log: &[u8],
    platform: PathPlatform,
    source_budget: usize,
    max_plaintext_bytes: usize,
) -> Result<StagedLog, RestoreRefusal> {
    use RestoreRefusal::Zstd;
    let (frames, torn_start) = scan_frames(log, usize::MAX).map_err(Zstd)?;
    let Some(first) = frames.first() else {
        return Err(Zstd(ZstdRefusal::Empty));
    };
    let mut used = 0;
    let header = decode(&log[first.clone()], true, &mut used, max_plaintext_bytes)
        .map_err(|error| error.at(first.start, max_plaintext_bytes))?;
    if header.is_empty() || header.iter().position(|byte| *byte == b'\n') != Some(header.len() - 1)
    {
        return Err(Zstd(ZstdRefusal::HeaderFrame));
    }
    let mut scanner =
        LogScanner::new(&header, platform, source_budget).map_err(RestoreRefusal::Scan)?;
    for frame in &frames[1..] {
        let plaintext = decode(&log[frame.clone()], true, &mut used, max_plaintext_bytes)
            .map_err(|error| error.at(frame.start, max_plaintext_bytes))?;
        scanner.feed(&plaintext).map_err(RestoreRefusal::Scan)?;
    }
    let (input_bytes, committed_bytes, recovered_from) = scanner.checkpoint();
    if input_bytes != committed_bytes {
        return Err(Zstd(ZstdRefusal::CompleteFramesUncommitted));
    }
    let torn = if let Some(truncate_to) = torn_start {
        match decode(&log[truncate_to..], false, &mut used, max_plaintext_bytes) {
            Ok(plaintext) => scanner.feed(&plaintext).map_err(RestoreRefusal::Scan)?,
            Err(DecodeError::Frame) => {}
            Err(DecodeError::Budget) => {
                return Err(RestoreRefusal::NativePlaintextBudget {
                    max_plaintext_bytes,
                });
            }
        }
        Some(TornTail {
            truncate_to,
            recovered_from,
        })
    } else {
        None
    };
    Ok(StagedLog::new(
        scanner.finish().map_err(RestoreRefusal::Scan)?,
        torn,
    ))
}

/// The first frame's plaintext, as TypeScript's `readFirstZstdLine` reads a
/// generation header without inspecting the frames after it: `None` when no
/// complete first frame is present, otherwise one LF-terminated record.
/// Refuses with `Zstd` for an invalid first frame or one that is not exactly
/// one header line, and with `NativePlaintextBudget` past
/// `max_plaintext_bytes`.
pub fn zstd_header_record(
    log: &[u8],
    max_plaintext_bytes: usize,
) -> Result<Option<Vec<u8>>, RestoreRefusal> {
    let (frames, _) = scan_frames(log, 1).map_err(RestoreRefusal::Zstd)?;
    let Some(first) = frames.first() else {
        return Ok(None);
    };
    let header = decode(&log[first.clone()], true, &mut 0, max_plaintext_bytes)
        .map_err(|error| error.at(first.start, max_plaintext_bytes))?;
    if header.is_empty() || header.iter().position(|byte| *byte == b'\n') != Some(header.len() - 1)
    {
        return Err(RestoreRefusal::Zstd(ZstdRefusal::HeaderFrame));
    }
    Ok(Some(header))
}

type Frames = (Vec<std::ops::Range<usize>>, Option<usize>);

/// Structural frame ranges and the torn final frame's start, stopping after
/// `max_frames` complete frames as TypeScript's `scanZstdFrames` does.
fn scan_frames(bytes: &[u8], max_frames: usize) -> Result<Frames, ZstdRefusal> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let start = offset;
        if bytes.len() - offset < 4 {
            return Ok((frames, Some(start)));
        }
        if bytes[offset..offset + 4] != [0x28, 0xb5, 0x2f, 0xfd] {
            return Err(ZstdRefusal::Magic { offset });
        }
        offset += 4;
        if offset == bytes.len() {
            return Ok((frames, Some(start)));
        }
        let descriptor = bytes[offset];
        offset += 1;
        if descriptor & 0x18 != 0 {
            return Err(ZstdRefusal::ReservedHeaderBit { offset: offset - 1 });
        }
        let single_segment = descriptor & 0x20 != 0;
        let size_flag = descriptor >> 6;
        let dict_flag = descriptor & 3;
        let dictionary_bytes = if dict_flag == 3 {
            4
        } else {
            usize::from(dict_flag)
        };
        let content_size_bytes = if size_flag == 0 {
            usize::from(single_segment)
        } else {
            1 << size_flag
        };
        let remaining = usize::from(!single_segment) + dictionary_bytes + content_size_bytes;
        if bytes.len() - offset < remaining {
            return Ok((frames, Some(start)));
        }
        offset += remaining;
        loop {
            if bytes.len() - offset < 3 {
                return Ok((frames, Some(start)));
            }
            let block =
                u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], 0]);
            offset += 3;
            let kind = (block >> 1) & 3;
            if kind == 3 {
                return Err(ZstdRefusal::ReservedBlockType { offset: offset - 3 });
            }
            let payload = if kind == 1 { 1 } else { (block >> 3) as usize };
            if bytes.len() - offset < payload {
                return Ok((frames, Some(start)));
            }
            offset += payload;
            if block & 1 != 0 {
                break;
            }
        }
        if descriptor & 4 != 0 {
            if bytes.len() - offset < 4 {
                return Ok((frames, Some(start)));
            }
            offset += 4;
        }
        frames.push(start..offset);
        if frames.len() == max_frames {
            return Ok((frames, None));
        }
    }
    Ok((frames, None))
}

#[derive(Debug)]
enum DecodeError {
    Frame,
    Budget,
}

impl DecodeError {
    fn at(self, start: usize, max_plaintext_bytes: usize) -> RestoreRefusal {
        match self {
            Self::Frame => RestoreRefusal::Zstd(ZstdRefusal::Frame { start }),
            Self::Budget => RestoreRefusal::NativePlaintextBudget {
                max_plaintext_bytes,
            },
        }
    }
}

fn decode(
    bytes: &[u8],
    complete: bool,
    used: &mut usize,
    max: usize,
) -> Result<Vec<u8>, DecodeError> {
    let mut decoder = DCtx::try_create().ok_or(DecodeError::Frame)?;
    let mut input = InBuffer::around(bytes);
    let mut scratch = [0u8; 65536];
    let mut plaintext = Vec::new();
    loop {
        let before = input.pos();
        let mut output = OutBuffer::around(&mut scratch[..]);
        let hint = decoder
            .decompress_stream(&mut output, &mut input)
            .map_err(|_| DecodeError::Frame)?;
        let count = output.pos();
        if count > max.saturating_sub(*used) {
            return Err(DecodeError::Budget);
        }
        *used += count;
        plaintext.extend_from_slice(&scratch[..count]);
        // Completion can fill the buffer exactly. Another call would start
        // a new frame and falsely report truncated input.
        if hint == 0 {
            return if input.pos() == bytes.len() {
                Ok(plaintext)
            } else {
                Err(DecodeError::Frame)
            };
        }
        if input.pos() == bytes.len() && count == 0 {
            return if complete {
                Err(DecodeError::Frame)
            } else {
                Ok(plaintext)
            };
        }
        if before == input.pos() && count == 0 {
            return Err(DecodeError::Frame);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn decoder_version_matches_the_qualified_vendored_library() {
        assert_eq!(zstd_safe::version_number(), 10507);
    }
}
