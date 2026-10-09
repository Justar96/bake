//! Zstd log framing and plaintext recovery, for current-format logs and for
//! the in-memory migration of released v0 to v2 generations. Structural validation
//! precedes decoding; complete frames are decoded before their plaintext is
//! scanned. A torn-frame decoding error discards all output from that frame.

use zstd_safe::{DCtx, InBuffer, OutBuffer};

use crate::scan::LogScanner;
use crate::{
    MigratedV2, PathPlatform, ReleasedGenerationRefusal, RestoreRefusal, RestoredLog, StagedLog,
    TornTail, plain_log_file::migrate_released_generation_before_stop,
};

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

/// The [`ReleasedGenerationRefusal::Limit`] name of
/// [`released_zstd_plaintext`]'s plaintext budget.
pub const RELEASED_ZSTD_PLAINTEXT_BUDGET: &str = "zstd/plaintext-budget";

/// The plaintext of a Zstd v0, v1, or v2 generation, decoded as the read
/// `open`'s `decodeStreamingMigration` decodes it; see
/// [`released_zstd_plaintext`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedZstdPlaintext {
    /// The header frame's one record, then the complete records the body
    /// streamed: every complete frame's plaintext and, when nothing stopped
    /// it, the complete records recovered from a torn final frame. It is a
    /// plain log for [`crate::released_generation_header`] and
    /// [`crate::migrate_released_generation`].
    pub plaintext: Vec<u8>,
    /// What TypeScript throws after streaming `plaintext`'s rows: a complete
    /// body frame that fails to decode, complete frames that end inside a
    /// record, or this crate's plaintext budget. `None` when the body ended.
    pub stop: Option<ReleasedGenerationRefusal>,
}

/// Decode an in-memory Zstd v0, v1, or v2 generation as the read `open`'s
/// `decodeStreamingMigration` does before and while it streams rows.
///
/// The whole file's structural frame scan and the header frame, which must
/// be exactly one record, are refused here, before the header is read, as
/// TypeScript throws them; each is [`ReleasedGenerationRefusal::Corrupt`]
/// with its `String(error)`. The body's own stop is returned with the
/// records before it, for [`migrate_released_zstd_generation`] to order
/// after the stored identity check. `max_plaintext_bytes` bounds cumulative
/// decoder output; exceeding it is the [`RELEASED_ZSTD_PLAINTEXT_BUDGET`]
/// limit, which TypeScript does not have.
/// Nothing is read from or written to a file.
pub fn released_zstd_plaintext(
    log: &[u8],
    max_plaintext_bytes: usize,
) -> Result<ReleasedZstdPlaintext, ReleasedGenerationRefusal> {
    let corrupt = |refusal: ZstdRefusal| {
        ReleasedGenerationRefusal::Corrupt(format!("Error: {}", refusal.message()))
    };
    let budget = || ReleasedGenerationRefusal::Limit(RELEASED_ZSTD_PLAINTEXT_BUDGET.to_owned());
    let (frames, torn_start) = scan_frames(log, usize::MAX).map_err(corrupt)?;
    let Some(first) = frames.first() else {
        return Err(corrupt(ZstdRefusal::Empty));
    };
    let mut used = 0;
    let mut plaintext = match decode(&log[first.clone()], true, &mut used, max_plaintext_bytes) {
        Ok(header) => header,
        Err(DecodeError::Frame) => return Err(corrupt(ZstdRefusal::Frame { start: first.start })),
        Err(DecodeError::Budget) => return Err(budget()),
    };
    if plaintext.is_empty()
        || plaintext.iter().position(|byte| *byte == b'\n') != Some(plaintext.len() - 1)
    {
        return Err(corrupt(ZstdRefusal::HeaderFrame));
    }
    // The rows before a stop streamed; a record it cut never did.
    let stopped = |mut plaintext: Vec<u8>, stop| {
        let end = plaintext
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |last| last + 1);
        plaintext.truncate(end);
        Ok(ReleasedZstdPlaintext {
            plaintext,
            stop: Some(stop),
        })
    };
    for frame in &frames[1..] {
        match decode(&log[frame.clone()], true, &mut used, max_plaintext_bytes) {
            Ok(decoded) => plaintext.extend_from_slice(&decoded),
            Err(DecodeError::Frame) => {
                return stopped(
                    plaintext,
                    corrupt(ZstdRefusal::Frame { start: frame.start }),
                );
            }
            Err(DecodeError::Budget) => return stopped(plaintext, budget()),
        }
    }
    if plaintext.last() != Some(&b'\n') {
        return stopped(plaintext, corrupt(ZstdRefusal::CompleteFramesUncommitted));
    }
    if let Some(start) = torn_start {
        // `decompressZstdPrefix` failing recovers nothing.
        match decode(&log[start..], false, &mut used, max_plaintext_bytes) {
            Ok(recovered) => {
                if let Some(last) = recovered.iter().rposition(|byte| *byte == b'\n') {
                    plaintext.extend_from_slice(&recovered[..=last]);
                }
            }
            Err(DecodeError::Frame) => {}
            Err(DecodeError::Budget) => return stopped(plaintext, budget()),
        }
    }
    Ok(ReleasedZstdPlaintext {
        plaintext,
        stop: None,
    })
}

/// [`crate::migrate_released_generation`] over a decoded Zstd generation
/// whose header [`crate::released_generation_header`] admitted.
///
/// TypeScript's `decodeStreamingMigration` throws a body stop after the rows
/// before it streamed and before the decoders' and the chain's `finish`, so
/// the stop is the plain migration's parse stop: a refusal thrown while
/// those rows stream wins, and the stop wins over a `finish` refusal.
pub fn migrate_released_zstd_generation(
    decoded: &ReleasedZstdPlaintext,
    source_version: u64,
    source_budget: usize,
) -> Result<MigratedV2, ReleasedGenerationRefusal> {
    migrate_released_generation_before_stop(
        &decoded.plaintext,
        source_version,
        source_budget,
        decoded.stop.clone(),
    )
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
