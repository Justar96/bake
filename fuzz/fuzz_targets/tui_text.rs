//! Untrusted text through `bake-tui-view`'s wrapping, layout, and styling:
//! model output, tool output, and pasted drafts all reach these functions.
//!
//! Input: byte 0 is the width (1 to 256 cells), byte 1 the caret seed, the
//! rest UTF-8 text (lossy). Oracles: wrapped rows fit the width (a space past
//! a row's end hangs) except a lone grapheme wider than it, layout rows are
//! ordered char-boundary ranges that do not depend on the caret, and every
//! styled range lies on char boundaries of the text it styles.

#![no_main]

use std::ops::Range;

use bake_tui_view::activity::Tones;
use bake_tui_view::diff;
use bake_tui_view::editor::{self, cell_width};
use bake_tui_view::selection;
use bake_tui_view::shell_output::{self, Family};
use bake_tui_view::syntax::{self, Carry, Lang};
use bake_tui_view::transcript::wrap;
use libfuzzer_sys::fuzz_target;
use unicode_segmentation::UnicodeSegmentation;

/// The cells a wrapped row fills, as the layout counts them: a space at or
/// past the row's end hangs and fills none.
fn row_cells(row: &str, width: usize) -> usize {
    row.graphemes(true).fold(0, |col, grapheme| match grapheme {
        " " => col + usize::from(col < width),
        _ => col + cell_width(grapheme),
    })
}

fn assert_ranges<T>(text: &str, ranges: &[(Range<usize>, T)], what: &str) {
    for (range, _) in ranges {
        assert!(
            range.start <= range.end
                && range.end <= text.len()
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end),
            "{what} range {range:?} is not a char range of {text:?}"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let [width, caret_seed, rest @ ..] = data else {
        return;
    };
    let width = usize::from(*width) + 1;
    let text = String::from_utf8_lossy(rest);
    let text = text.as_ref();

    for row in wrap(text, width) {
        assert!(!row.contains('\n'), "a wrapped row holds a line break");
        // Only a lone grapheme wider than the row may overfill it.
        let mut filled = row
            .graphemes(true)
            .filter(|grapheme| *grapheme != " " && cell_width(grapheme) > 0);
        let lone_wide = matches!(
            (filled.next(), filled.next()),
            (Some(grapheme), None) if cell_width(grapheme) > width
        );
        assert!(
            row_cells(&row, width) <= width || lone_wide,
            "wrapped row {row:?} is wider than {width}"
        );
    }

    // The editor keeps its caret on a char boundary; only the text is
    // untrusted.
    let mut caret = (usize::from(*caret_seed) * text.len()) / 255;
    while !text.is_char_boundary(caret) {
        caret -= 1;
    }
    let layout = editor::layout(text, caret, width);
    assert_eq!(
        layout.rows,
        editor::layout(text, 0, width).rows,
        "layout rows depend on the caret"
    );
    let mut previous_end = 0;
    for row in &layout.rows {
        assert!(
            previous_end <= row.start
                && row.start <= row.end
                && row.end <= text.len()
                && text.is_char_boundary(row.start)
                && text.is_char_boundary(row.end),
            "layout row {row:?} is not an ordered char range of {text:?}"
        );
        previous_end = row.end;
        let _ = editor::display(&text[row.start..row.end], width);
    }
    let _ = editor::sanitize(text);
    let _ = editor::word_stop(text, caret, caret_seed % 2 == 0, &[]);

    let lines: Vec<&str> = text.split('\n').collect();
    let family = match caret_seed % 4 {
        0 => Family::Search,
        1 => Family::Test,
        2 => Family::GitStatus,
        _ => Family::Other,
    };
    let lang = match caret_seed % 3 {
        0 => Lang::TypeScript,
        1 => Lang::Rust,
        _ => Lang::Shell,
    };
    let mut carry = Carry::default();
    for line in &lines {
        let (clean, runs) = shell_output::parse_sgr(line, Tones::TrueColor);
        assert_ranges(&clean, &runs, "SGR");
        let styled = shell_output::styled(line, family, Tones::Ansi);
        assert_ranges(&styled.text, &styled.runs, "styled");
        let marks = shell_output::marks(line, family);
        assert_ranges(line, &marks, "mark");
        let tokens = syntax::tokens(line, lang, &mut carry);
        assert_ranges(line, &tokens, "syntax");
        let _ = shell_output::family(line);
        let _ = selection::word_at(line, width);
        let _ = selection::slice_cells(line, usize::from(*caret_seed), width);
    }

    let owned: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    let rows = diff::parse(&owned);
    let _ = diff::split(&rows);
    let _ = diff::number_width(&rows);
    if let [a, b, ..] = lines.as_slice()
        && let Some((left, right)) = diff::changed(a, b)
    {
        assert!(a.get(left).is_some() && b.get(right).is_some());
    }
});
