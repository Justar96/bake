//! Isolated probe: does ratatui-textarea 0.9.3 reproduce Bake's pure editor oracles?
//! Every expectation below is copied from apps/tui/packages/ui/tests/editor.test.ts
//! (cited per check). Mismatches are recorded, never weakened; exit code 1 if any.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use unicode_width::UnicodeWidthStr;

fn ta(text: &str, width: u16) -> TextArea<'static> {
    let mut t = TextArea::from(text.split('\n').map(str::to_owned).collect::<Vec<_>>());
    t.set_wrap_mode(WrapMode::WordOrGlyph);
    t.set_tab_length(4);
    t.set_cursor_line_style(ratatui::style::Style::default());
    render(&t, width);
    t
}

/// Rendering records the area that the screen map (wrap, Up/Down) uses.
fn render(t: &TextArea, width: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, 12);
    let mut buf = Buffer::empty(area);
    t.render(area, &mut buf);
    (0..area.height)
        .map(|y| {
            // A wide grapheme's trailing cells are placeholders, not text.
            let (mut row, mut x) = (String::new(), 0);
            while x < width {
                let sym = buf[(x, y)].symbol();
                row.push_str(sym);
                x += sym.width().max(1) as u16;
            }
            row.trim_end().to_owned()
        })
        .collect()
}

/// Text before the caret, comparable with Bake's `text.slice(0, cursor)`.
fn prefix(t: &TextArea) -> String {
    let (row, col) = (t.cursor().0, t.cursor().1);
    let mut s: Vec<String> = t.lines()[..row].to_vec();
    s.push(t.lines()[row].chars().take(col).collect());
    s.join("\n")
}

fn text(t: &TextArea) -> String { t.lines().join("\n") }

/// Place the caret after `before`, which must be a prefix of the text.
fn at(text: &str, before: &str, width: u16) -> TextArea<'static> {
    assert!(text.starts_with(before));
    let row = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap().chars().count();
    let mut t = ta(text, width);
    t.move_cursor(CursorMove::Jump(row as u16, col as u16));
    render(&t, width);
    t
}

struct Report { fails: usize }
impl Report {
    fn check(&mut self, id: &str, oracle: &str, got: impl std::fmt::Debug, want: impl std::fmt::Debug) {
        let (g, w) = (format!("{got:?}"), format!("{want:?}"));
        let ok = g == w;
        if !ok { self.fails += 1 }
        println!("{} {id}\n    oracle: {oracle}\n    want:   {w}\n    got:    {g}", if ok { "PASS" } else { "FAIL" });
    }
    fn note(&self, id: &str, msg: impl std::fmt::Display) { println!("INFO {id}: {msg}") }
}

fn main() {
    let mut r = Report { fails: 0 };
    let w = 40;

    // 1. Grapheme movement and deletion (editor.test.ts:30-40, :24-28, :55-66).
    let s = "a👩🏽‍💻e\u{301}z";
    let mut t = at(s, "a", w);
    t.move_cursor(CursorMove::Forward);
    r.check("G1 right over emoji ZWJ+skin tone", "editor.test.ts:33-34", prefix(&t), "a👩🏽‍💻");
    let mut t = at(s, "a👩🏽‍💻", w);
    t.delete_char();
    r.check("G2 backspace emoji", "editor.test.ts:36", (text(&t), prefix(&t)), ("ae\u{301}z", "a"));
    let mut t = at(s, "a👩🏽‍💻", w);
    t.delete_next_char();
    r.check("G3 forward-delete e+combining acute", "editor.test.ts:35", text(&t), "a👩🏽‍💻z");
    for cluster in ["น้ำ", "ກຳ"] {
        let s = format!("a{cluster}z");
        let mut t = at(&s, "a", w);
        t.move_cursor(CursorMove::Forward);
        r.check(&format!("G4 right over {cluster}"), "editor.test.ts:59", prefix(&t), format!("a{cluster}"));
        let mut t = at(&s, &format!("a{cluster}"), w);
        t.delete_char();
        r.check(&format!("G5 backspace {cluster}"), "editor.test.ts:61", text(&t), "az");
    }
    // Insertion that joins a grapheme leaves the caret after it (editor.test.ts:51).
    let mut t = at("👩💻", "👩", w);
    t.insert_char('\u{200d}');
    r.check("G6 ZWJ insertion caret outside cluster", "editor.test.ts:51", (text(&t), prefix(&t)), ("👩‍💻", "👩‍💻"));

    // 2. One cell width for caret and wrap (caret.test.ts, editor.test.ts:94-125).
    let t = at("x👩🏽‍💻", "x👩🏽‍💻", w);
    r.note("W0", format!("unicode-width 0.2.2 str width of 👩🏽‍💻 = {}, per-char sum = {}",
        "👩🏽‍💻".width(), "👩🏽‍💻".chars().map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)).sum::<usize>()));
    r.note("W1 ratatui buffer row", format!("{:?}", render(&t, w)[0]));
    r.check("W1 caret cell after x+emoji (Bake: 1 + 2 cells)", "editor.test.ts:20 / caret.test.ts:9 (string-width = 2)",
        t.screen_cursor().col, 3usize);
    // Bake lays out one column narrower than the row (editor.test.ts:72), so render at width-1.
    for (s, bake_width, want) in [
        ("alpha beta gamma", 11u16, vec!["alpha beta", "gamma"]),
        ("你好世界你好", 6, vec!["你好", "世界", "你好"]),
        ("ภาษาไทยทดสอบ", 9, vec!["ภาษาไทย", "ทดสอบ"]),
        ("ພາສາລາວທົດສອບ", 9, vec!["ພາສາລາວ", "ທົດສອບ"]),
    ] {
        let t = ta(s, bake_width - 1);
        let rows: Vec<String> = render(&t, bake_width - 1).into_iter().filter(|l| !l.is_empty()).collect();
        r.check(&format!("W2 wrap {s:?} at {bake_width}"), "editor.test.ts:73,108,119-120", rows, want);
    }

    // 3. Visual-row navigation (editor.test.ts:171-212).
    let s = "alpha beta gamma";
    let mut t = at(s, "al", 10);
    t.move_cursor(CursorMove::Down);
    r.check("V1 down within wrapped line", "editor.test.ts:174", prefix(&t), "alpha beta ga");
    let s = "abcd\n你好";
    let mut t = at(s, "abc", w);
    t.move_cursor(CursorMove::Down);
    r.check("V2 down never splits wide char", "editor.test.ts:192", prefix(&t), "abcd\n你");
    let s = "long line here\nab\nanother long line";
    let mut t = at(s, "long line ", w);
    t.move_cursor(CursorMove::Down);
    render(&t, w);
    t.move_cursor(CursorMove::Down);
    r.check("V3 goal column survives short row", "editor.test.ts:205-212", prefix(&t), "long line here\nab\nanother lo");
    let mut t = at("one\ntwo", "on", w);
    let before = t.cursor();
    t.move_cursor(CursorMove::Up);
    r.note("V4 first row Up", format!("cursor {:?} -> {:?} (no-op; Bake returns undefined so history recalls, editor.test.ts:183)", before, t.cursor()));

    // 4. Word stops (editor.test.ts:241-251), walking left with WordBack.
    for (s, want) in [
        ("src/main.ts --fix", vec!["src/main.ts --|fix", "src/main.ts |--fix", "src/main.|ts --fix", "src/main|.ts --fix", "src/|main.ts --fix", "src|/main.ts --fix", "|src/main.ts --fix"]),
        ("你好世界 hi", vec!["你好世界 |hi", "你好|世界 hi", "|你好世界 hi"]),
    ] {
        let mut t = at(s, s, w);
        let mut stops = Vec::new();
        loop {
            let before = t.cursor();
            t.move_cursor(CursorMove::WordBack);
            if t.cursor() == before { break }
            let p = prefix(&t);
            stops.push(format!("{p}|{}", &s[p.len()..]));
        }
        r.check(&format!("K1 word-left walk {s:?}"), "editor.test.ts:242-250", stops, want);
    }

    // 5. Default bindings: input() vs Bake's composer (composer.ts:61-63, :311-321).
    for (name, code, mods, bake) in [
        ("Enter", KeyCode::Enter, KeyModifiers::NONE, "submit"),
        ("Ctrl-U", KeyCode::Char('u'), KeyModifiers::CONTROL, "kill to line start"),
        ("Ctrl-J", KeyCode::Char('j'), KeyModifiers::CONTROL, "insert newline"),
        ("Ctrl-Y", KeyCode::Char('y'), KeyModifiers::CONTROL, "yank kill ring"),
    ] {
        let mut t = at("ab\ncd", "ab\nc", w);
        t.insert_char('x');
        t.set_yank_text("Y");
        let modified = t.input(KeyEvent::new(code, mods));
        r.note(&format!("B {name}"), format!("input() modified={modified} -> {:?}|{:?}  (Bake: {bake})", prefix(&t), &text(&t)[prefix(&t).len()..]));
    }
    let mut t = at("ab", "ab", w);
    t.input_without_shortcuts(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    r.note("B input_without_shortcuts Ctrl-U", format!("text {:?} (ignored, so Bake can own it)", text(&t)));

    println!("\n{} mismatch(es) against Bake oracles", r.fails);
    std::process::exit(if r.fails == 0 { 0 } else { 1 });
}
