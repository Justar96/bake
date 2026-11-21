//! OSC 52 terminal clipboard framing and transport selection.
//!
//! - Plain terminals and `tmux` receive raw BEL-terminated OSC 52
//!   (`\x1b]52;c;<base64>\x07`). Under `tmux`, raw OSC 52 respects the user's
//!   `set-clipboard` policy without bypassing it via DCS passthrough.
//! - GNU Screen (`STY` without a `tmux` marker) drops raw OSC 52 and enforces
//!   a 768-byte `MAXSTR` limit per DCS string, so the BEL-terminated OSC 52
//!   sequence is split into `\x1bP<chunk>\x1b\\` frames of at most
//!   [`SCREEN_DCS_CHUNK_BYTES`] bytes.
//! - `TERM=dumb` disables terminal copy altogether.
//! - In ambiguous nested multiplexer sessions where both `tmux` and `STY`
//!   markers are present, `tmux` takes precedence so DCS passthrough is never
//!   emitted into `tmux`.

use base64::Engine as _;

/// Maximum raw UTF-8 byte length sent over OSC 52 (64 KiB).
///
/// Checked before allocating the base64 payload or framed sequence. Oversized
/// selections return `None` rather than truncating. Local native clipboard
/// helpers are not subject to this cap.
pub(crate) const MAX_OSC52_RAW_BYTES: usize = 64 * 1024;

/// Maximum payload bytes inside one GNU Screen DCS passthrough frame
/// (`\x1bP<chunk>\x1b\\`), kept below Screen's 768-byte `MAXSTR` limit so
/// Screen reconstructs the inner OSC 52 sequence without spilling bytes.
const SCREEN_DCS_CHUNK_BYTES: usize = 700;

const OSC52_PREFIX: &str = "\x1b]52;c;";
const OSC52_SUFFIX: &str = "\x07";
const DCS_START: &[u8] = b"\x1bP";
const DCS_END: &[u8] = b"\x1b\\";

/// How OSC 52 sequences are framed for the attached terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TerminalTransport {
    /// Raw BEL-terminated OSC 52 (`\x1b]52;c;<base64>\x07`).
    Plain,
    /// Chunked GNU Screen DCS passthrough around BEL-terminated OSC 52.
    ScreenDcs,
    /// Terminal cannot accept OSC 52 sequences (e.g. `TERM=dumb`).
    Unsupported,
}

impl TerminalTransport {
    /// Chooses the terminal clipboard transport from environment variables.
    pub(super) fn detect(env: &impl Fn(&str) -> Option<String>) -> Self {
        let non_empty = |name: &str| env(name).filter(|value| !value.is_empty());

        if non_empty("TERM").is_some_and(|term| term.eq_ignore_ascii_case("dumb")) {
            return Self::Unsupported;
        }

        let in_tmux = non_empty("TMUX").is_some()
            || non_empty("TERM_PROGRAM")
                .is_some_and(|program| program.eq_ignore_ascii_case("tmux"));
        if in_tmux {
            return Self::Plain;
        }

        if non_empty("STY").is_some() {
            return Self::ScreenDcs;
        }

        Self::Plain
    }

    /// Encodes `text` for this transport, or returns `None` when the terminal
    /// is unsupported or `text` exceeds [`MAX_OSC52_RAW_BYTES`].
    pub(super) fn encode(self, text: &str) -> Option<String> {
        if matches!(self, Self::Unsupported) || text.len() > MAX_OSC52_RAW_BYTES {
            return None;
        }
        let inner = plain_osc52(text);
        match self {
            Self::Plain => Some(inner),
            Self::ScreenDcs => wrap_screen_dcs(&inner),
            Self::Unsupported => None,
        }
    }
}

/// Encodes `text` as a raw BEL-terminated OSC 52 sequence using standard Base64.
pub(super) fn plain_osc52(text: &str) -> String {
    let encoded_len = base64::encoded_len(text.len(), true).unwrap_or(0);
    let mut out = String::with_capacity(OSC52_PREFIX.len() + encoded_len + OSC52_SUFFIX.len());
    out.push_str(OSC52_PREFIX);
    base64::engine::general_purpose::STANDARD.encode_string(text.as_bytes(), &mut out);
    out.push_str(OSC52_SUFFIX);
    out
}

/// Wraps a pure-ASCII BEL-terminated OSC 52 sequence in GNU Screen DCS
/// passthrough frames of at most [`SCREEN_DCS_CHUNK_BYTES`] bytes each.
fn wrap_screen_dcs(inner: &str) -> Option<String> {
    let bytes = inner.as_bytes();
    let chunks = bytes.len().div_ceil(SCREEN_DCS_CHUNK_BYTES);
    let mut out = Vec::with_capacity(bytes.len() + chunks * (DCS_START.len() + DCS_END.len()));
    for chunk in bytes.chunks(SCREEN_DCS_CHUNK_BYTES) {
        out.extend_from_slice(DCS_START);
        out.extend_from_slice(chunk);
        out.extend_from_slice(DCS_END);
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn detect(vars: &[(&str, &str)]) -> TerminalTransport {
        TerminalTransport::detect(&|name| {
            vars.iter()
                .find(|&&(k, _)| k == name)
                .map(|&(_, v)| v.to_owned())
        })
    }

    fn unwrap_screen_dcs(framed: &str) -> Vec<&str> {
        let mut rest = framed;
        let mut chunks = Vec::new();
        while !rest.is_empty() {
            let after_start = rest
                .strip_prefix("\x1bP")
                .expect("each DCS frame starts with ESC P");
            let end = after_start
                .find("\x1b\\")
                .expect("each DCS frame ends with ST");
            let body = &after_start[..end];
            assert!(!body.is_empty() && body.len() <= SCREEN_DCS_CHUNK_BYTES);
            assert!(!body.contains("\x1bP"));
            chunks.push(body);
            rest = &after_start[end + 2..];
        }
        chunks
    }

    #[test]
    fn transport_priority_table() {
        // Priority: TERM=dumb > tmux markers (TMUX / TERM_PROGRAM=tmux) > STY > Plain.
        let cases: &[(&[(&str, &str)], TerminalTransport)] = &[
            (&[], TerminalTransport::Plain),
            (&[("TERM", "xterm-256color")], TerminalTransport::Plain),
            (
                &[("TMUX", "/tmp/tmux-1000/default,1,0")],
                TerminalTransport::Plain,
            ),
            (&[("TERM_PROGRAM", "tmux")], TerminalTransport::Plain),
            (&[("STY", "1234.pts-0.host")], TerminalTransport::ScreenDcs),
            (
                &[("TMUX", ""), ("STY", "1234.pts-0.host")],
                TerminalTransport::ScreenDcs,
            ),
            (&[("STY", "")], TerminalTransport::Plain),
            (
                &[("TMUX", "/tmp/t"), ("STY", "1.pts")],
                TerminalTransport::Plain,
            ),
            (
                &[("TERM_PROGRAM", "tmux"), ("STY", "1.pts")],
                TerminalTransport::Plain,
            ),
            (&[("TERM", "dumb")], TerminalTransport::Unsupported),
            (
                &[("TERM", "DUMB"), ("TMUX", "/tmp/t")],
                TerminalTransport::Unsupported,
            ),
            (
                &[("TERM", "dumb"), ("STY", "1.pts")],
                TerminalTransport::Unsupported,
            ),
        ];
        for &(vars, expected) in cases {
            assert_eq!(detect(vars), expected, "vars={vars:?}");
        }
    }

    #[test]
    fn exact_framing_small_vectors_and_control_neutralization() {
        assert_eq!(
            TerminalTransport::Plain.encode("").as_deref(),
            Some("\x1b]52;c;\x07")
        );
        assert_eq!(
            TerminalTransport::Plain.encode("hi").as_deref(),
            Some("\x1b]52;c;aGk=\x07")
        );
        assert_eq!(
            TerminalTransport::Plain.encode("é漢😀").as_deref(),
            Some("\x1b]52;c;w6nmvKLwn5iA\x07")
        );
        assert_eq!(
            TerminalTransport::ScreenDcs.encode("hi").as_deref(),
            Some("\x1bP\x1b]52;c;aGk=\x07\x1b\\")
        );

        let hostile = "\x1b]52;c;evil\x07\x1b\\\x1bPtmux;\x00\r\n";
        let framed = TerminalTransport::Plain.encode(hostile).unwrap();
        let payload = framed
            .strip_prefix("\x1b]52;c;")
            .and_then(|s| s.strip_suffix('\x07'))
            .unwrap();
        assert!(
            payload
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        );
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap(),
            hostile.as_bytes()
        );
    }

    #[test]
    fn screen_dcs_chunk_and_raw_cap_boundaries() {
        // (raw_len, expected_dcs_frames, last_frame_len)
        let cases = [
            (519, 1, SCREEN_DCS_CHUNK_BYTES),
            (520, 2, 4),
            (568, 2, 68),
            (MAX_OSC52_RAW_BYTES, 125, 592),
        ];
        for (raw_len, expected_frames, last_len) in cases {
            let text = "a".repeat(raw_len);
            let plain = TerminalTransport::Plain.encode(&text).unwrap();
            let screen = TerminalTransport::ScreenDcs.encode(&text).unwrap();
            let chunks = unwrap_screen_dcs(&screen);
            assert_eq!(chunks.len(), expected_frames, "raw_len={raw_len}");
            assert_eq!(chunks.last().unwrap().len(), last_len, "raw_len={raw_len}");
            assert_eq!(chunks.concat(), plain, "raw_len={raw_len}");
        }

        let too_large = "x".repeat(MAX_OSC52_RAW_BYTES + 1);
        assert_eq!(TerminalTransport::Plain.encode(&too_large), None);
        assert_eq!(TerminalTransport::ScreenDcs.encode(&too_large), None);
        assert_eq!(TerminalTransport::Unsupported.encode("hi"), None);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn osc52_never_injects_controls_and_round_trips_through_screen_dcs(
            text in ".{0,2048}"
        ) {
            prop_assume!(text.len() <= MAX_OSC52_RAW_BYTES);

            let plain = TerminalTransport::Plain.encode(&text).unwrap();
            let payload = plain
                .strip_prefix("\x1b]52;c;")
                .and_then(|s| s.strip_suffix('\x07'))
                .unwrap();
            prop_assert!(
                payload
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
            );
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap();
            prop_assert_eq!(decoded, text.as_bytes());

            let screen = TerminalTransport::ScreenDcs.encode(&text).unwrap();
            prop_assert_eq!(unwrap_screen_dcs(&screen).concat(), plain);
        }
    }
}
