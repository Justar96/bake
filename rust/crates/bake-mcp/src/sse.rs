//! A bounded, incremental server-sent events decoder.
//!
//! Ported from `consumeSseStream` in Pi's Streamable HTTP transport
//! (`packages/mcp/src/transports/streamable-http.ts`, v1.3.0): comments are
//! skipped, one space after a field's colon is dropped, `data` lines join
//! with `\n`, an `id` without NUL updates the stream's last event ID at once
//! (also for an event without data, which primes resumption), an all-digit
//! `retry` sets the reconnection delay, an event without data is not
//! dispatched, and at the end of the stream an unfinished line is processed
//! and a pending event dispatched, as Pi does.
//!
//! Differences from Pi, each from the event stream format of the WHATWG HTML
//! standard: a bare `\r` also ends a line, and a `\r\n` split across chunks
//! is one line break. The decoder works on bytes, not decoded text: line
//! breaks never occur inside a UTF-8 sequence, so a sequence split across
//! chunks needs no joining, and `data` stays bytes for the JSON parser, which
//! rejects invalid UTF-8. A leading byte order mark is dropped, as
//! `TextDecoder` drops it.
//!
//! # Bounds
//!
//! The decoder holds at most one unfinished line and one event's data. A line
//! may be `data_limit` plus [`LINE_OVERHEAD`] bytes, room for a field name
//! before a full-size value, and an event's data (with its `\n` joins)
//! `data_limit` bytes. Both are checked before bytes are copied, so a peer
//! that never ends a line or an event fails with [`TooLarge`] once the limit
//! is passed instead of growing a buffer. The decoder does not read; the
//! caller passes chunks it already holds and may await between items, which
//! is how a slow consumer holds back the stream.

use reqwest::header::HeaderValue;

/// Bytes a line may hold beyond the data limit: a field name, its colon, and
/// the optional space.
pub(crate) const LINE_OVERHEAD: usize = 64;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// A line or an event over its limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TooLarge {
    pub(crate) limit: usize,
}

/// One decoded item, in stream order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SseItem {
    /// An `id` field. Values that are not valid header values (Pi skips only
    /// values with NUL) are dropped, since they could not be sent back as
    /// `Last-Event-ID`.
    Id(HeaderValue),
    /// An all-digit `retry` field that fits in `u64`, in milliseconds.
    Retry(u64),
    /// A dispatched event that has data.
    Event {
        /// Whether the event type is `message` (absent or empty counts).
        message: bool,
        /// The data lines, joined with `\n`.
        data: Vec<u8>,
    },
}

#[derive(Debug)]
pub(crate) struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    message: bool,
    /// The previous chunk ended with `\r`, so a leading `\n` belongs to it.
    after_cr: bool,
    /// Bytes of a possible leading BOM seen so far; `None` once decided.
    bom: Option<usize>,
    data_limit: usize,
    line_limit: usize,
}

impl SseDecoder {
    pub(crate) fn new(data_limit: usize) -> Self {
        Self {
            line: Vec::new(),
            data: Vec::new(),
            has_data: false,
            message: true,
            after_cr: false,
            bom: Some(0),
            data_limit,
            line_limit: data_limit.saturating_add(LINE_OVERHEAD),
        }
    }

    /// The next item from `input`, consuming the bytes it was decoded from.
    /// Returns `Ok(None)` once `input` is empty. Call again until it returns
    /// `None` before passing the next chunk.
    pub(crate) fn next(&mut self, input: &mut &[u8]) -> Result<Option<SseItem>, TooLarge> {
        loop {
            self.skip_bom(input);
            if self.after_cr && !input.is_empty() {
                self.after_cr = false;
                if input[0] == b'\n' {
                    *input = &input[1..];
                }
            }
            if input.is_empty() {
                return Ok(None);
            }
            let Some(end) = input.iter().position(|byte| matches!(byte, b'\n' | b'\r')) else {
                self.append(input.len(), input)?;
                *input = &[];
                return Ok(None);
            };
            self.append(end, input)?;
            let terminator = input[end];
            let mut rest = &input[end + 1..];
            if terminator == b'\r' {
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    Some(_) => {}
                    None => self.after_cr = true,
                }
            }
            *input = rest;
            let line = std::mem::take(&mut self.line);
            let item = self.process(&line);
            self.line = line;
            self.line.clear();
            if let Some(item) = item? {
                return Ok(Some(item));
            }
        }
    }

    /// The end of the stream: process an unfinished line and dispatch a
    /// pending event, as Pi does. Returns at most two items.
    pub(crate) fn finish(&mut self) -> Result<Vec<SseItem>, TooLarge> {
        let mut items = Vec::new();
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            if let Some(item) = self.process(&line)? {
                items.push(item);
            }
        }
        if let Some(item) = self.dispatch() {
            items.push(item);
        }
        Ok(items)
    }

    fn skip_bom(&mut self, input: &mut &[u8]) {
        let Some(seen) = self.bom else {
            return;
        };
        let wanted = &BOM[seen..];
        let matched = wanted
            .iter()
            .zip(input.iter())
            .take_while(|(a, b)| a == b)
            .count();
        if matched == wanted.len() {
            *input = &input[matched..];
            self.bom = None;
        } else if matched == input.len() {
            // The chunk ended inside a possible BOM.
            *input = &[];
            self.bom = Some(seen + matched);
        } else {
            // Not a BOM: bytes held back from earlier chunks are line bytes.
            self.line.extend_from_slice(&BOM[..seen]);
            self.bom = None;
        }
    }

    fn append(&mut self, count: usize, input: &[u8]) -> Result<(), TooLarge> {
        if self.line.len().saturating_add(count) > self.line_limit {
            return Err(TooLarge {
                limit: self.data_limit,
            });
        }
        self.line.extend_from_slice(&input[..count]);
        Ok(())
    }

    fn process(&mut self, line: &[u8]) -> Result<Option<SseItem>, TooLarge> {
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        if line[0] == b':' {
            return Ok(None);
        }
        let (field, mut value) = match line.iter().position(|byte| *byte == b':') {
            Some(colon) => (&line[..colon], &line[colon + 1..]),
            None => (line, &[][..]),
        };
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        match field {
            b"data" => {
                let join = usize::from(self.has_data);
                if self
                    .data
                    .len()
                    .saturating_add(join)
                    .saturating_add(value.len())
                    > self.data_limit
                {
                    return Err(TooLarge {
                        limit: self.data_limit,
                    });
                }
                if self.has_data {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
                self.has_data = true;
            }
            b"event" => self.message = value.is_empty() || value == b"message",
            b"id" if !value.contains(&0) => {
                if let Ok(id) = HeaderValue::from_bytes(value) {
                    return Ok(Some(SseItem::Id(id)));
                }
            }
            b"retry" if !value.is_empty() && value.iter().all(u8::is_ascii_digit) => {
                // All ASCII digits, so it is UTF-8; only overflow fails.
                if let Some(delay) = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse().ok())
                {
                    return Ok(Some(SseItem::Retry(delay)));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<SseItem> {
        let message = std::mem::replace(&mut self.message, true);
        if !std::mem::take(&mut self.has_data) {
            return None;
        }
        Some(SseItem::Event {
            message,
            data: std::mem::take(&mut self.data),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&[u8]], limit: usize) -> Result<Vec<SseItem>, TooLarge> {
        let mut decoder = SseDecoder::new(limit);
        let mut items = Vec::new();
        for chunk in chunks {
            let mut input = *chunk;
            while let Some(item) = decoder.next(&mut input)? {
                items.push(item);
            }
            assert!(input.is_empty());
        }
        items.extend(decoder.finish()?);
        Ok(items)
    }

    fn event(data: &str) -> SseItem {
        SseItem::Event {
            message: true,
            data: data.as_bytes().to_vec(),
        }
    }

    fn id(value: &str) -> SseItem {
        SseItem::Id(HeaderValue::from_str(value).unwrap())
    }

    #[test]
    fn parses_chunked_crlf_events_comments_ids_and_multiline_data() {
        // Pi: "parses chunked CRLF events, comments, IDs, and multiline data".
        let items = decode(
            &[
                b": keepalive\r\nid: 7\r\ndata: {\"one\":\r\n",
                b"data: 1}\r\n\r\n",
            ],
            1024,
        )
        .unwrap();
        assert_eq!(items, [id("7"), event("{\"one\":\n1}")]);
    }

    #[test]
    fn every_split_point_and_line_ending_decodes_the_same() {
        let text =
            "\u{FEFF}id: é1\nretry: 25\ndata: {\"a\":\"😀\"}\n\nevent: ping\ndata: x\n\ndata:\n\n";
        let expected = [
            id("é1"),
            SseItem::Retry(25),
            event("{\"a\":\"😀\"}"),
            SseItem::Event {
                message: false,
                data: b"x".to_vec(),
            },
            event(""),
        ];
        for ending in ["\n", "\r\n", "\r"] {
            let input = text.replace('\n', ending);
            let bytes = input.as_bytes();
            for split in 0..=bytes.len() {
                let (first, second) = bytes.split_at(split);
                let items = decode(&[first, second], 1024).unwrap();
                assert_eq!(items, expected, "ending {ending:?}, split at {split}");
            }
            // One byte at a time.
            let chunks: Vec<&[u8]> = bytes.chunks(1).collect();
            assert_eq!(decode(&chunks, 1024).unwrap(), expected, "{ending:?}");
        }
    }

    #[test]
    fn invalid_utf8_stays_in_the_data_for_the_json_parser_to_refuse() {
        let items = decode(&[b"data: \xFF\xFE\n\n"], 64).unwrap();
        assert_eq!(
            items,
            [SseItem::Event {
                message: true,
                data: b"\xFF\xFE".to_vec()
            }]
        );
    }

    #[test]
    fn ids_with_nul_or_control_bytes_and_bad_retries_are_ignored() {
        let items = decode(
            &[b"id: a\0b\nid: c\x01d\nretry: 1x\nretry: 99999999999999999999999\nretry:\ndata: y\n\n"],
            64,
        )
        .unwrap();
        assert_eq!(items, [event("y")]);
    }

    #[test]
    fn an_event_without_data_is_not_dispatched_but_its_id_counts() {
        // A priming event: an ID and empty data are both present in Pi's
        // fixture ("id: 1\nretry: 5\ndata:\n\n") and dispatch an empty event;
        // an ID alone does not dispatch.
        let items = decode(&[b"id: 9\n\n"], 64).unwrap();
        assert_eq!(items, [id("9")]);
    }

    #[test]
    fn the_stream_end_processes_the_last_line_and_dispatches() {
        let items = decode(&[b"data: tail"], 64).unwrap();
        assert_eq!(items, [event("tail")]);
        assert_eq!(
            decode(&[b"data: a\nid: 3"], 64).unwrap(),
            [id("3"), event("a")]
        );
    }

    #[test]
    fn data_lines_past_the_limit_fail_without_a_blank_line() {
        // Pi: "rejects events whose data lines exceed the limit without a
        // blank line". Each line is short; only their sum passes the limit.
        let mut decoder = SseDecoder::new(256);
        let mut fed = 0;
        let error = loop {
            fed += 1;
            assert!(fed < 100, "the decoder kept accepting data lines");
            let mut input = &b"data: xxxxxxxxxxxxxxxx\n"[..];
            match decoder.next(&mut input) {
                Ok(None) => {}
                Ok(Some(item)) => panic!("dispatched {item:?}"),
                Err(error) => break error,
            }
        };
        assert_eq!(error, TooLarge { limit: 256 });
        assert!(decoder.data.len() <= 256);
    }

    #[test]
    fn a_line_past_the_limit_fails_before_it_is_buffered() {
        let mut decoder = SseDecoder::new(16);
        let limit = 16 + LINE_OVERHEAD;
        let mut input = vec![b'x'; limit];
        let mut slice = &input[..];
        assert_eq!(decoder.next(&mut slice), Ok(None));
        assert_eq!(decoder.line.len(), limit);
        input.truncate(1);
        let mut slice = &input[..];
        assert_eq!(decoder.next(&mut slice), Err(TooLarge { limit: 16 }));
        assert_eq!(decoder.line.len(), limit, "the refused byte was buffered");
        // A data value at the limit fits with its field name.
        let line = format!("data: {}\n\n", "y".repeat(16));
        assert_eq!(
            decode(&[line.as_bytes()], 16).unwrap(),
            [event(&"y".repeat(16))]
        );
        let line = format!("data: {}\n\n", "y".repeat(17));
        assert_eq!(decode(&[line.as_bytes()], 16), Err(TooLarge { limit: 16 }));
    }

    #[test]
    fn a_bom_is_dropped_only_at_the_start() {
        assert_eq!(
            decode(&[b"\xEF\xBB", b"\xBFdata: a\n\n"], 64).unwrap(),
            [event("a")]
        );
        // A partial BOM that turns out not to be one is kept as line bytes.
        assert_eq!(
            decode(&[b"\xEF", b"data: a\n\n"], 64).unwrap(),
            Vec::<SseItem>::new()
        );
        assert_eq!(
            decode(&[b"data: \xEF\xBB\xBFa\n\n"], 64).unwrap(),
            [SseItem::Event {
                message: true,
                data: b"\xEF\xBB\xBFa".to_vec()
            }]
        );
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        /// Every item a stream decodes to, in order, and the error that
        /// ended it, if any.
        fn decode_items(chunks: &[&[u8]], limit: usize) -> (Vec<SseItem>, Option<TooLarge>) {
            let mut decoder = SseDecoder::new(limit);
            let mut items = Vec::new();
            for chunk in chunks {
                let mut input = *chunk;
                loop {
                    match decoder.next(&mut input) {
                        Ok(Some(item)) => items.push(item),
                        Ok(None) => break,
                        Err(error) => return (items, Some(error)),
                    }
                }
            }
            match decoder.finish() {
                Ok(rest) => {
                    items.extend(rest);
                    (items, None)
                }
                Err(error) => (items, Some(error)),
            }
        }

        fn fragment() -> impl Strategy<Value = Vec<u8>> {
            prop_oneof![
                Just(b"data:".to_vec()),
                Just(b"data: ".to_vec()),
                Just(b"event: ".to_vec()),
                Just(b"event: message".to_vec()),
                Just(b"id: ".to_vec()),
                Just(b"retry: ".to_vec()),
                Just(b": ping".to_vec()),
                Just(b"\n".to_vec()),
                Just(b"\r".to_vec()),
                Just(b"\r\n".to_vec()),
                Just(b"\xEF\xBB\xBF".to_vec()),
                Just(b"\xEF\xBB".to_vec()),
                "[0-9]{1,3}".prop_map(String::into_bytes),
                "\\PC{0,8}".prop_map(String::into_bytes),
                prop::collection::vec(any::<u8>(), 0..4),
            ]
        }

        /// A stream that may open with all or part of a BOM.
        fn stream() -> impl Strategy<Value = Vec<u8>> {
            let opening = prop_oneof![
                Just(&b""[..]),
                Just(&b"\xEF"[..]),
                Just(&b"\xEF\xBB"[..]),
                Just(&b"\xEF\xBB\xBF"[..]),
            ];
            (opening, prop::collection::vec(fragment(), 0..40))
                .prop_map(|(opening, parts)| [opening.to_vec(), parts.concat()].concat())
        }

        /// Cut points, half of them in the first bytes, where the decoder
        /// decides whether the stream opens with a BOM.
        fn cuts() -> impl Strategy<Value = Vec<usize>> {
            prop::collection::vec(prop_oneof![0..4usize, any::<usize>()], 0..8)
        }

        fn chunks<'a>(bytes: &'a [u8], cuts: &[usize]) -> Vec<&'a [u8]> {
            let mut cuts: Vec<usize> = cuts.iter().map(|cut| cut % (bytes.len() + 1)).collect();
            cuts.sort_unstable();
            let mut chunks = Vec::new();
            let mut start = 0;
            for cut in cuts {
                chunks.push(&bytes[start..cut]);
                start = cut;
            }
            chunks.push(&bytes[start..]);
            chunks
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            #[test]
            fn any_split_of_a_stream_decodes_to_the_same_items_and_limit_error(
                bytes in stream(),
                cuts in cuts(),
                limit in prop_oneof![0..24usize, Just(1024usize)],
            ) {
                let whole = decode_items(&[&bytes], limit);
                prop_assert_eq!(decode_items(&chunks(&bytes, &cuts), limit), whole);
            }

        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn no_decoded_event_holds_more_data_than_the_limit(
                bytes in stream(),
                cuts in cuts(),
                limit in 0..24usize,
            ) {
                let (items, _) = decode_items(&chunks(&bytes, &cuts), limit);
                for item in items {
                    if let SseItem::Event { data, .. } = item {
                        prop_assert!(data.len() <= limit, "{} bytes over {limit}", data.len());
                    }
                }
            }

            #[test]
            fn events_written_with_any_line_ending_decode_to_their_ids_and_data(
                events in prop::collection::vec(
                    (
                        prop::option::of("[!-~]{1,6}"),
                        prop::option::of(prop_oneof![Just("message"), Just("ping")]),
                        prop::collection::vec("[^\r\n]{0,12}", 1..4),
                    ),
                    0..6,
                ),
                ending in prop_oneof![Just("\n"), Just("\r"), Just("\r\n")],
                cuts in cuts(),
            ) {
                let mut text = String::new();
                let mut expected = Vec::new();
                for (id, event, lines) in &events {
                    if let Some(id) = id {
                        text.push_str(&format!("id: {id}{ending}"));
                        expected.push(SseItem::Id(HeaderValue::from_str(id).unwrap()));
                    }
                    if let Some(event) = event {
                        text.push_str(&format!("event: {event}{ending}"));
                    }
                    for line in lines {
                        text.push_str(&format!("data: {line}{ending}"));
                    }
                    text.push_str(ending);
                    expected.push(SseItem::Event {
                        message: event.is_none_or(|event| event == "message"),
                        data: lines.join("\n").into_bytes(),
                    });
                }
                let decoded = decode_items(&chunks(text.as_bytes(), &cuts), 1024);
                prop_assert_eq!(decoded, (expected, None));
            }
        }
    }
}
