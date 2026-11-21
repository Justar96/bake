//! Public-behavior tests for transcript selection copy fidelity and
//! copy-to-paste round-trips.

use std::time::Duration;

use bake_tui_view::keys::{Key, KeyInput};
use bake_tui_view::render::render;
use bake_tui_view::state::{Effect, Mouse, MouseKind, Msg, State, update};
use bake_tui_view::transcript::Row;
use proptest::prelude::*;
use ratatui_core::backend::TestBackend;
use ratatui_core::terminal::Terminal;

fn draw(app: &mut State, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

fn mouse(kind: MouseKind, column: u16, row: u16, ms: u64) -> Msg {
    Msg::Mouse(Mouse {
        kind,
        column,
        row,
        alt: false,
        at: Duration::from_millis(ms),
    })
}

fn find(rows: &[String], needle: &str) -> (u16, u16) {
    for (y, line) in rows.iter().enumerate() {
        if let Some(byte) = line.find(needle) {
            let col = unicode_width::UnicodeWidthStr::width(&line[..byte]);
            return (u16::try_from(y).unwrap(), u16::try_from(col).unwrap());
        }
    }
    panic!("needle {needle:?} not found in:\n{}", rows.join("\n"));
}

fn find_last(rows: &[String], needle: &str) -> (u16, u16) {
    for (y, line) in rows.iter().enumerate().rev() {
        if let Some(byte) = line.rfind(needle) {
            let col = unicode_width::UnicodeWidthStr::width(&line[..byte]);
            return (u16::try_from(y).unwrap(), u16::try_from(col).unwrap());
        }
    }
    panic!("needle {needle:?} not found in:\n{}", rows.join("\n"));
}

fn drag_copy(app: &mut State, from: (u16, u16), to: (u16, u16)) -> Option<String> {
    update(app, mouse(MouseKind::Down, from.0, from.1, 0));
    update(app, mouse(MouseKind::Drag, to.0, to.1, 10));
    update(app, mouse(MouseKind::Up, to.0, to.1, 20))
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Copy(text) => Some(text),
            _ => None,
        })
}

fn state_with_rows(rows: Vec<Row>) -> State {
    let mut app = State::default();
    app.transcript.replace(rows);
    app
}

#[test]
fn user_prompt_roundtrips_tabs_unicode_trailing_spaces_hard_blanks_and_soft_wraps() {
    let source = "alpha\tbeta   \n\ncafe\u{0301} 漢 🚀 long wrapped sentence that crosses multiple visual lines at narrow widths\n\tend";
    for width in [26u16, 80, 140] {
        let mut app = state_with_rows(vec![Row::User(source.into())]);
        let rows = draw(&mut app, width, 24);
        let (y_start, x_start) = find(&rows, "alpha");
        let (y_end, x_end) = find(&rows, "end");
        let copied = drag_copy(&mut app, (x_start, y_start), (x_end + 2, y_end));
        assert_eq!(copied.as_deref(), Some(source), "width={width}");

        // Round-trip directly back into the composer via bracketed paste.
        let paste_effects = update(&mut app, Msg::Paste(copied.unwrap()));
        assert!(
            paste_effects.is_empty(),
            "paste must not auto-submit (width={width})"
        );
        assert_eq!(app.draft.expanded(), source, "width={width}");
        let _ = update(&mut app, Msg::Key(KeyInput::plain(Key::Enter)));
        assert_eq!(app.draft.text(), "");
    }
}

#[test]
fn fenced_code_block_preserves_tabs_trailing_spaces_blanks_and_rejoins_soft_wraps() {
    let code = "\tfn demo() {\n\t\tlet msg = \"very_long_identifier_for_soft_wrap_check_1234567890\";   \n\n\t\tprintln!(\"{msg}\");\n\t}";
    let answer = format!("```rust\n{code}\n```");
    for width in [34u16, 80, 120] {
        let mut app = state_with_rows(vec![Row::Answer(answer.clone())]);
        let rows = draw(&mut app, width, 24);
        let (y_start, _) = find(&rows, "fn demo()");
        let (y_end, x_end) = find_last(&rows, "}");
        // Select from column 0 (inside the `  │ ` gutter) through the closing brace.
        let copied = drag_copy(&mut app, (0, y_start), (x_end, y_end));
        assert_eq!(copied.as_deref(), Some(code), "width={width}");
    }
}

#[test]
fn wrapped_markdown_prose_rejoins_without_spurious_newline_or_gutter_and_keeps_visual_text() {
    let answer = "A **bold** sentence with `inline` code that wraps cleanly across visual rows.\n\n- bullet item that also wraps across visual rows here";
    let mut app = state_with_rows(vec![Row::Answer(answer.into())]);
    let rows = draw(&mut app, 36, 24);
    let (y_start, x_start) = find(&rows, "A bold");
    let (y_end, x_end) = find(&rows, "here");
    let copied = drag_copy(&mut app, (x_start, y_start), (x_end + 3, y_end));
    assert_eq!(
        copied.as_deref(),
        Some(
            "A bold sentence with inline code that wraps cleanly across visual rows.\n\n• bullet item that also wraps across visual rows here"
        )
    );
}

#[test]
fn zwj_emoji_before_tab_stays_synced_across_wraps_and_wide_selection_widens() {
    let source = "👩‍💻\talpha_token_1234567890\n👨‍👩‍👧\tbeta";
    for width in [22u16, 80] {
        let mut app = state_with_rows(vec![Row::Answer(source.into())]);
        let rows = draw(&mut app, width, 24);
        let (y_start, _) = find(&rows, "👩‍💻");
        let (y_end, x_end) = find(&rows, "beta");
        let copied = drag_copy(&mut app, (0, y_start), (x_end + 3, y_end));
        assert_eq!(copied.as_deref(), Some(source), "width={width}");
    }

    // Cutting through the middle of wide CJK characters widens to include whole graphemes.
    let mut app = state_with_rows(vec![Row::User("你好世界".into())]);
    let rows = draw(&mut app, 80, 24);
    let (y, x) = find(&rows, "你");
    // `你` occupies x..x+2, `好` occupies x+2..x+4; dragging x+1..x+2 covers both.
    let copied = drag_copy(&mut app, (x + 1, y), (x + 2, y));
    assert_eq!(copied.as_deref(), Some("你好"));
}

#[test]
fn intentional_source_bom_is_preserved_at_start_middle_and_end() {
    let source = "\u{feff}lead \u{feff}mid\u{feff}";
    let mut app = state_with_rows(vec![Row::User(source.into())]);
    let rows = draw(&mut app, 80, 24);
    let (y, x) = find(&rows, "lead");
    // Selecting the whole line preserves all three intentional U+FEFF marks.
    let full = drag_copy(&mut app, (x, y), (x + 7, y));
    assert_eq!(full.as_deref(), Some(source));
    // Selecting only "ead" skips the leading BOM before 'l' and the mid BOM.
    let partial = drag_copy(&mut app, (x + 1, y), (x + 3, y));
    assert_eq!(partial.as_deref(), Some("ead"));
}

#[test]
fn whitespace_only_and_partial_trailing_whitespace_selections_are_preserved_without_fake_padding() {
    let mut app = state_with_rows(vec![Row::User("hello   \n   \n\t\nend".into())]);
    let rows = draw(&mut app, 80, 24);
    let (y0, x0) = find(&rows, "hello");
    // Partial trailing spaces: 2 of the 3 trailing spaces after "hello".
    let partial_ws = drag_copy(&mut app, (x0 + 5, y0), (x0 + 6, y0));
    assert_eq!(partial_ws.as_deref(), Some("  "));
    // Dragging well past the end of "hello   " keeps only the 3 real trailing spaces.
    let past_end = drag_copy(&mut app, (x0, y0), (x0 + 40, y0));
    assert_eq!(past_end.as_deref(), Some("hello   "));
    // Dragging strictly in empty terminal space past "hello   " copies nothing.
    let empty_space = drag_copy(&mut app, (x0 + 12, y0), (x0 + 25, y0));
    assert_eq!(empty_space, None);
    // Whitespace-only line (3 spaces).
    let ws_line = drag_copy(&mut app, (x0, y0 + 1), (x0 + 2, y0 + 1));
    assert_eq!(ws_line.as_deref(), Some("   "));
    // Tab-only line.
    let tab_line = drag_copy(&mut app, (x0, y0 + 2), (x0 + 1, y0 + 2));
    assert_eq!(tab_line.as_deref(), Some("\t"));
    // Dragging strictly inside the gutter copies nothing.
    let gutter_only = drag_copy(&mut app, (0, y0), (1, y0));
    assert_eq!(gutter_only, None);
    // Dragging from line 0 to the start of line 1 captures the trailing hard newline.
    let trailing_newline = drag_copy(&mut app, (x0, y0), (0, y0 + 1));
    assert_eq!(trailing_newline.as_deref(), Some("hello   \n"));
}

#[test]
fn reverse_selections_match_forward_selections_single_and_multiline() {
    let source = "first\tline   \n\nsecond wrapped line that spans multiple visual rows";
    let mut app = state_with_rows(vec![Row::User(source.into())]);
    let rows = draw(&mut app, 30, 24);
    let (y_start, x_start) = find(&rows, "first");
    let (y_end, x_end) = find(&rows, "rows");

    let forward = drag_copy(&mut app, (x_start, y_start), (x_end + 3, y_end));
    let reverse = drag_copy(&mut app, (x_end + 3, y_end), (x_start, y_start));
    assert_eq!(forward.as_deref(), Some(source));
    assert_eq!(reverse, forward);

    // Single-line right-to-left drag.
    let fwd_single = drag_copy(&mut app, (x_start, y_start), (x_start + 4, y_start));
    let rev_single = drag_copy(&mut app, (x_start + 4, y_start), (x_start, y_start));
    assert_eq!(fwd_single.as_deref(), Some("first"));
    assert_eq!(rev_single, fwd_single);
}

#[test]
fn raw_escape_and_control_characters_are_never_copied() {
    let untrusted = "safe \x1b[31mred\x1b[0m \x1b]52;c;Zm9v\x07\x00end";
    for row in [Row::User(untrusted.into()), Row::Error(untrusted.into())] {
        let mut app = state_with_rows(vec![row]);
        let rows = draw(&mut app, 80, 24);
        let (y, x) = find(&rows, "safe");
        let copied = drag_copy(&mut app, (x, y), (70, y)).expect("non-empty copy");
        assert!(
            copied
                .chars()
                .all(|c| c == '\t' || c == '\n' || !c.is_control()),
            "control char leaked into copy: {copied:?}"
        );
        assert!(copied.starts_with("safe "));
        assert!(copied.ends_with("end"));
    }
}

fn source_atom() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("abc"),
        Just("xy"),
        Just("longtoken1234567890"),
        Just(" "),
        Just("   "),
        Just("\t"),
        Just("\n"),
        Just("\n\n"),
        Just("漢"),
        Just("你好"),
        Just("e\u{0301}"),
        Just("a\u{0308}"),
        Just("👩‍💻"),
        Just("👨‍👩‍👧"),
        Just("\u{feff}"),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn source_backed_user_and_fenced_code_roundtrip_arbitrary_atoms_across_widths(
        atoms in prop::collection::vec(source_atom(), 0..16),
        width in 20u16..100u16,
    ) {
        let mut full = String::from("START");
        for atom in atoms {
            full.push_str(atom);
        }
        // A one-cell sentinel cannot straddle a soft wrap.
        full.push('Z');

        // 1. Row::User source-backed round-trip.
        let mut user_app = state_with_rows(vec![Row::User(full.clone())]);
        let user_rows = draw(&mut user_app, width, 64);
        let (y_start, x_start) = find(&user_rows, "START");
        let (y_end, _) = find_last(&user_rows, "Z");
        let copied_user = drag_copy(&mut user_app, (x_start, y_start), (width - 1, y_end));
        prop_assert_eq!(copied_user.as_deref(), Some(full.as_str()));

        // 2. Row::Answer fenced code block source-backed round-trip.
        let fenced = format!("```txt\n{full}\n```");
        let mut code_app = state_with_rows(vec![Row::Answer(fenced)]);
        let code_rows = draw(&mut code_app, width, 64);
        let (cy_start, _) = find(&code_rows, "START");
        let (cy_end, _) = find_last(&code_rows, "Z");
        let copied_code = drag_copy(&mut code_app, (0, cy_start), (width - 1, cy_end));
        prop_assert_eq!(copied_code.as_deref(), Some(full.as_str()));
    }
}
