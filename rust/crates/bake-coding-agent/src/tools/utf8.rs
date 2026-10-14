//! The streaming `TextDecoder` behavior Pi uses for process output.

#[derive(Debug, Default)]
pub(super) struct Decoder {
    pending: Vec<u8>,
    started: bool,
}

impl Decoder {
    pub(super) fn decode(&mut self, bytes: &[u8]) -> String {
        let mut joined;
        let mut rest = if self.pending.is_empty() {
            bytes
        } else {
            joined = std::mem::take(&mut self.pending);
            joined.extend_from_slice(bytes);
            &joined
        };
        let mut output = String::new();
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    output.push_str(text);
                    break;
                }
                Err(error) => {
                    let (valid, invalid) = rest.split_at(error.valid_up_to());
                    output.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match error.error_len() {
                        Some(length) => {
                            output.push('\u{FFFD}');
                            rest = &invalid[length..];
                        }
                        None => {
                            // Only an unfinished sequence survives a call, at most 3 bytes.
                            self.pending.extend_from_slice(invalid);
                            break;
                        }
                    }
                }
            }
        }
        if !self.started && !output.is_empty() {
            self.started = true;
            if output.starts_with('\u{FEFF}') {
                output.drain(..'\u{FEFF}'.len_utf8());
            }
        }
        output
    }

    pub(super) fn finish(&mut self) -> &'static str {
        if self.pending.is_empty() {
            ""
        } else {
            self.pending.clear();
            "\u{FFFD}"
        }
    }
}

pub(super) fn decode_all(bytes: &[u8]) -> String {
    let mut decoder = Decoder::default();
    let mut text = decoder.decode(bytes);
    text.push_str(decoder.finish());
    text
}
