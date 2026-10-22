//! Strict JSONL framing, ported from Pi v1.3.0
//! `packages/coding-agent/src/modes/rpc/jsonl.ts`
//! (`71023aec1546d6d677a65669c5665c0927ce0dd0`).
//!
//! Framing is LF-only: a payload string may hold other Unicode separators,
//! such as U+2028 and U+2029, so records split on `\n` alone and a trailing
//! `\r` is dropped, as Pi's reader does. Bytes are split before they are
//! decoded, so a UTF-8 sequence crossing a chunk boundary is kept whole
//! where Pi's `StringDecoder` keeps it; an undecodable byte becomes
//! U+FFFD, as that decoder's replacement does, rather than failing the
//! stream.

use std::io::Read;

use serde_json::Value;

/// Pi's `serializeJsonLine`: one record and its LF. `serde_json` escapes
/// no Unicode separator either, so the payload keeps U+2028 and U+2029.
pub fn serialize_json_line(value: &Value) -> String {
    format!("{value}\n")
}

/// Pi's `attachJsonlLineReader` buffer: bytes in, whole records out.
#[derive(Debug, Default)]
pub struct JsonlLines {
    buffer: Vec<u8>,
}

impl JsonlLines {
    /// A reader with nothing buffered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes `chunk` and calls `on_line` for each record it completes.
    pub fn push(&mut self, chunk: &[u8], mut on_line: impl FnMut(String)) {
        self.buffer.extend_from_slice(chunk);
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            on_line(decode(&line[..newline]));
        }
    }

    /// Pi's `end`: the last record, when the stream ended without an LF.
    pub fn finish(&mut self, mut on_line: impl FnMut(String)) {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            on_line(decode(&line));
        }
    }
}

/// A record's text: the line without its trailing `\r`, undecodable bytes
/// replaced.
fn decode(line: &[u8]) -> String {
    let line = match line.last() {
        Some(b'\r') => &line[..line.len() - 1],
        _ => line,
    };
    String::from_utf8_lossy(line).into_owned()
}

/// Reads `input` to its end, calling `on_line` for each record, including a
/// last one without an LF. A read failure ends the stream, as a closed pipe
/// ends Pi's.
pub fn read_lines(mut input: impl Read, mut on_line: impl FnMut(String)) {
    let mut lines = JsonlLines::new();
    let mut chunk = [0u8; 8192];
    loop {
        match input.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => lines.push(&chunk[..read], &mut on_line),
        }
    }
    lines.finish(&mut on_line);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut reader = JsonlLines::new();
        for chunk in chunks {
            reader.push(chunk, |line| lines.push(line));
        }
        reader.finish(|line| lines.push(line));
        lines
    }

    // Pi `test/rpc-jsonl.test.ts`: "serializes strict JSONL records without
    // escaping Unicode separators".
    #[test]
    fn serializes_strict_jsonl_records_without_escaping_unicode_separators() {
        let line = serialize_json_line(&json!({ "text": "a\u{2028}b\u{2029}c" }));
        assert!(line.contains("a\u{2028}b\u{2029}c"));
        assert!(line.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<Value>(line.trim_end()).expect("record"),
            json!({ "text": "a\u{2028}b\u{2029}c" })
        );
    }

    // Pi `test/rpc-jsonl.test.ts`: "splits on LF only and preserves
    // U+2028/U+2029 inside payloads".
    #[test]
    fn splits_on_lf_only_and_preserves_unicode_separators_inside_payloads() {
        let record = serialize_json_line(&json!({ "text": "a\u{2028}b\u{2029}c" }));
        let lines = collect(&[record.as_bytes()]);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&lines[0]).expect("record"),
            json!({ "text": "a\u{2028}b\u{2029}c" })
        );
    }

    // Pi `test/rpc-jsonl.test.ts`: "handles CRLF-delimited input".
    #[test]
    fn handles_crlf_delimited_input() {
        assert_eq!(
            collect(&[b"{\"a\":1}\r\n{\"b\":2}\r\n"]),
            ["{\"a\":1}", "{\"b\":2}"]
        );
    }

    // Pi `test/rpc-jsonl.test.ts`: "emits a final line without trailing LF".
    #[test]
    fn emits_a_final_line_without_trailing_lf() {
        assert_eq!(collect(&[b"{\"a\":1}"]), ["{\"a\":1}"]);
    }

    // Bake: a record split across chunks, inside a multi-byte character,
    // is one record with that character whole.
    #[test]
    fn a_record_split_inside_a_character_is_rejoined() {
        let text = "é\u{2028}ok";
        let record = serialize_json_line(&json!({ "text": text }));
        let bytes = record.as_bytes();
        for split in 1..bytes.len() {
            let lines = collect(&[&bytes[..split], &bytes[split..]]);
            assert_eq!(lines.len(), 1, "split at {split}");
            assert_eq!(
                serde_json::from_str::<Value>(&lines[0]).expect("record")["text"],
                json!(text),
                "split at {split}"
            );
        }
    }

    // Bake: an undecodable byte never fails the stream.
    #[test]
    fn an_undecodable_byte_becomes_the_replacement_character() {
        assert_eq!(collect(&[b"a\xffb\n"]), ["a\u{fffd}b"]);
    }

    #[test]
    fn read_lines_reads_a_stream_to_its_end() {
        let mut lines = Vec::new();
        read_lines(&b"{\"a\":1}\n{\"b\":2}"[..], |line| lines.push(line));
        assert_eq!(lines, ["{\"a\":1}", "{\"b\":2}"]);
    }
}
