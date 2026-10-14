//! Removal of unpaired UTF-16 surrogates before text reaches a provider.
//!
//! Ported from Pi `packages/ai/src/utils/sanitize-unicode.ts` (v1.1.0). A
//! Rust `str` is valid UTF-8 and cannot hold an unpaired surrogate, so text
//! that reaches a provider already satisfies Pi's guarantee and this returns
//! it unchanged. Surrogate escapes in parsed provider JSON are handled where
//! the text enters, by [`crate::utils::json_parse::repair_json`].

/// Returns `text` without unpaired surrogates; for a Rust `str`, unchanged.
pub fn sanitize_surrogates(text: &str) -> &str {
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pi's documented examples: paired surrogates (emoji) are preserved.
    #[test]
    fn keeps_valid_text_and_emoji() {
        assert_eq!(sanitize_surrogates("Hello 🙈 World"), "Hello 🙈 World");
        let lossy = String::from_utf16_lossy(&[0x54, 0xD83D, 0x20]);
        assert_eq!(sanitize_surrogates(&lossy), "T\u{FFFD} ");
    }
}
