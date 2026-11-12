//! Untrusted Mermaid source through `bake-mermaid`, as a model's answer
//! reaches it in a fenced block.
//!
//! Input: byte 0 is the width (0 to 255 cells), the rest UTF-8 source
//! (lossy). Oracles: rendering never panics; a drawing is not empty, every
//! line fits the width, and no span is empty or holds a control character;
//! the plain text matches the spans.

#![no_main]

use bake_mermaid::{render, render_spans};
use libfuzzer_sys::fuzz_target;
use unicode_width::UnicodeWidthStr;

fuzz_target!(|data: &[u8]| {
    let [width, rest @ ..] = data else {
        return;
    };
    let width = usize::from(*width);
    let source = String::from_utf8_lossy(rest);
    let spans = render_spans(&source, width);
    let text = render(&source, width);
    match (spans, text) {
        (Ok(lines), Ok(text)) => {
            assert!(!lines.is_empty(), "an empty drawing");
            let joined: Vec<String> = lines
                .iter()
                .map(|line| line.iter().map(|span| span.text.as_str()).collect())
                .collect();
            assert_eq!(joined.join("\n"), text, "the text differs from the spans");
            for (line, spans) in joined.iter().zip(&lines) {
                assert!(line.width() <= width, "{line:?} is wider than {width}");
                assert!(
                    !line.chars().any(char::is_control),
                    "{line:?} holds a control character"
                );
                assert!(
                    spans.iter().all(|span| !span.text.is_empty()),
                    "an empty span"
                );
            }
        }
        (Err(a), Err(b)) => assert_eq!(a, b, "the two renderers disagree"),
        (spans, text) => panic!("the two renderers disagree: {spans:?} and {text:?}"),
    }
});
