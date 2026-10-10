//! A server-sent events decoder over streamed bytes.
//!
//! Ported from the line decoder in Pi `packages/ai/src/api/anthropic-messages.ts`
//! (`decodeSseLine`, `consumeLine`, `iterateSseMessages`, v1.1.0), which
//! this crate uses for all three protocols. Bytes decode as UTF-8 the way
//! `TextDecoder` does in streaming mode: a sequence split across chunks is
//! joined, an invalid sequence becomes U+FFFD, and a leading BOM is dropped.
//! Unlike Pi's decoder, a CR at the end of a chunk waits for the next chunk,
//! so a CRLF split across chunks is one line break, not two.

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSentEvent {
    /// The `event` field, if any.
    pub event: Option<String>,
    /// The `data` lines joined by newlines.
    pub data: String,
    /// The event's raw lines.
    pub raw: Vec<String>,
}

#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
    started: bool,
}

impl Utf8StreamDecoder {
    fn decode(&mut self, bytes: &[u8], out: &mut String) {
        self.pending.extend_from_slice(bytes);
        let mut rest: &[u8] = &self.pending;
        let mut consumed = 0;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    consumed += rest.len();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.push_str(
                        std::str::from_utf8(rest.get(..valid).unwrap_or(&[])).unwrap_or(""),
                    );
                    match error.error_len() {
                        Some(invalid) => {
                            out.push('\u{FFFD}');
                            consumed += valid + invalid;
                            rest = rest.get(valid + invalid..).unwrap_or(&[]);
                        }
                        None => {
                            consumed += valid;
                            break;
                        }
                    }
                }
            }
        }
        self.pending.drain(..consumed.min(self.pending.len()));
        if !self.started && !out.is_empty() {
            self.started = true;
            if out.starts_with('\u{FEFF}') {
                out.drain(..'\u{FEFF}'.len_utf8());
            }
        }
    }

    fn finish(&mut self, out: &mut String) {
        if !self.pending.is_empty() {
            self.pending.clear();
            out.push('\u{FFFD}');
        }
    }
}

/// Incremental SSE decoder: feed byte chunks, collect events.
#[derive(Debug, Default)]
pub struct SseDecoder {
    utf8: Utf8StreamDecoder,
    buffer: String,
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

impl SseDecoder {
    /// A decoder at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes `bytes` and returns the events they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<ServerSentEvent> {
        let mut text = String::new();
        self.utf8.decode(bytes, &mut text);
        self.buffer.push_str(&text);
        let mut events = Vec::new();
        self.drain_lines(false, &mut events);
        events
    }

    /// Ends the stream: the last unterminated line and pending event.
    pub fn finish(&mut self) -> Vec<ServerSentEvent> {
        let mut text = String::new();
        self.utf8.finish(&mut text);
        self.buffer.push_str(&text);
        let mut events = Vec::new();
        self.drain_lines(true, &mut events);
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            events.extend(self.decode_line(line));
        }
        events.extend(self.flush());
        events
    }

    fn drain_lines(&mut self, at_end: bool, events: &mut Vec<ServerSentEvent>) {
        loop {
            let Some(index) = self.buffer.find(['\r', '\n']) else {
                return;
            };
            let is_cr = self.buffer.as_bytes().get(index) == Some(&b'\r');
            let mut next = index + 1;
            if is_cr {
                match self.buffer.as_bytes().get(next) {
                    Some(b'\n') => next += 1,
                    None if !at_end => return,
                    _ => {}
                }
            }
            let line = self.buffer.get(..index).unwrap_or("").to_owned();
            self.buffer.drain(..next);
            events.extend(self.decode_line(line));
        }
    }

    fn flush(&mut self) -> Option<ServerSentEvent> {
        if self.event.is_none() && self.data.is_empty() {
            return None;
        }
        Some(ServerSentEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data).join("\n"),
            raw: std::mem::take(&mut self.raw),
        })
    }

    fn decode_line(&mut self, line: String) -> Option<ServerSentEvent> {
        if line.is_empty() {
            return self.flush();
        }
        if line.starts_with(':') {
            self.raw.push(line);
            return None;
        }
        let (field, value) = match line.find(':') {
            None => (line.as_str(), ""),
            Some(index) => (
                line.get(..index).unwrap_or(""),
                line.get(index + 1..).unwrap_or(""),
            ),
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        self.raw.push(line);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(chunks: &[&[u8]]) -> Vec<ServerSentEvent> {
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.push(chunk));
        }
        events.extend(decoder.finish());
        events
    }

    fn data(events: &[ServerSentEvent]) -> Vec<(Option<&str>, &str)> {
        events
            .iter()
            .map(|event| (event.event.as_deref(), event.data.as_str()))
            .collect()
    }

    #[test]
    fn decodes_events_comments_and_multiline_data() {
        let events = decode_all(&[b": ping\nevent: a\ndata: 1\ndata:2\n\ndata: x\nid: 3\n\n"]);
        assert_eq!(data(&events), [(Some("a"), "1\n2"), (None, "x")]);
        assert_eq!(events[0].raw, [": ping", "event: a", "data: 1", "data:2"]);
    }

    #[test]
    fn handles_every_line_break_and_split_points() {
        let text = "event: e\r\ndata: one\r\n\r\ndata: two\r\rdata: three\n\ndata: tail";
        let whole = decode_all(&[text.as_bytes()]);
        assert_eq!(
            data(&whole),
            [
                (Some("e"), "one"),
                (None, "two"),
                (None, "three"),
                (None, "tail")
            ]
        );
        let bytes = text.as_bytes();
        for split in 0..=bytes.len() {
            let (left, right) = bytes.split_at(split);
            assert_eq!(decode_all(&[left, right]), whole, "split at {split}");
        }
    }

    #[test]
    fn joins_utf8_split_across_chunks_and_replaces_invalid_bytes() {
        let text = "data: héllo 🙈\n\n".as_bytes();
        for split in 0..=text.len() {
            let (left, right) = text.split_at(split);
            assert_eq!(data(&decode_all(&[left, right])), [(None, "héllo 🙈")]);
        }
        assert_eq!(
            data(&decode_all(&[b"data: a\xffb\n\n"])),
            [(None, "a\u{FFFD}b")]
        );
        assert_eq!(
            data(&decode_all(&[b"data: a\xf0\x9f"])),
            [(None, "a\u{FFFD}")]
        );
        assert_eq!(
            data(&decode_all(&[b"\xef\xbb\xbfdata: bom\n\n"])),
            [(None, "bom")]
        );
    }
}
