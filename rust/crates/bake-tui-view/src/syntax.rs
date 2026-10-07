//! Syntax colour for the code the transcript shows: a code-mode script's
//! source and result, a shell call's command, and the lines of an edit's
//! diff. Program output and prose are never coloured, because they are not
//! code in a known language.
//!
//! The lexers are small and line-based on purpose. They split a line into
//! tokens without changing a byte of it, so colour can never change a cell
//! width or a wrap, and a line they do not understand reads as plain text.
//! A block comment or template string carries from one line to the next.

use std::ops::Range;

use ratatui_core::style::{Color, Modifier, Style};

use crate::activity::Tones;

/// A language the transcript can colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    /// TypeScript and JavaScript; also what a code-mode script is written in.
    TypeScript,
    Rust,
    Shell,
}

/// The language of a file, by its extension; `None` draws it plain.
pub fn lang_for_path(path: &str) -> Option<Lang> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = name.rsplit_once('.').map(|(_, e)| e)?;
    match extension {
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "json" => {
            Some(Lang::TypeScript)
        }
        "rs" => Some(Lang::Rust),
        "sh" | "bash" | "zsh" => Some(Lang::Shell),
        _ => None,
    }
}

/// What a run of text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Plain,
    Keyword,
    String,
    /// Numbers and constants such as `true` and `null`.
    Number,
    Comment,
    /// A name being called: `read` in `tools.read(`.
    Call,
    /// A shell pipe, redirect, or separator.
    Operator,
    /// The program a shell command runs.
    Command,
    /// A shell option such as `-n` or `--frozen-lockfile`.
    Flag,
    /// A shell variable, `$HOME`, or an assignment's name.
    Variable,
}

/// A token's style. Each colour comes from the TypeScript palette's light
/// tones, so it reads on the call's box and on the terminal's own
/// background; a comment and a flag recede instead of taking a colour.
pub fn style(token: Token, tones: Tones) -> Style {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let (rgb, ansi) = match token {
        Token::Plain => return Style::new(),
        Token::Comment => return dim.add_modifier(Modifier::ITALIC),
        Token::Flag => return dim,
        Token::Keyword | Token::Operator => ((0xc4, 0xb5, 0xfd), Color::Magenta),
        Token::String => ((0xbe, 0xf2, 0x64), Color::Green),
        Token::Number | Token::Variable => ((0xfd, 0xba, 0x74), Color::Yellow),
        Token::Call | Token::Command => ((0x7d, 0xd3, 0xfc), Color::Cyan),
    };
    match tones {
        Tones::TrueColor => Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        Tones::Ansi => Style::new().fg(ansi),
        Tones::None => Style::new(),
    }
}

/// What a line leaves open for the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Carry {
    block_comment: bool,
    template: bool,
}

/// The tokens of `line`, as byte ranges that cover it in order. Plain runs
/// are left out. `carry` holds what an earlier line left open and is
/// updated for the next.
pub fn tokens(line: &str, lang: Lang, carry: &mut Carry) -> Vec<(Range<usize>, Token)> {
    let mut out = match lang {
        Lang::Shell => shell(line),
        Lang::TypeScript | Lang::Rust => c_like(line, lang, carry),
    };
    out.retain(|(range, token)| *token != Token::Plain && !range.is_empty());
    // Adjacent runs of one token are one span.
    let mut merged: Vec<(Range<usize>, Token)> = Vec::with_capacity(out.len());
    for (range, token) in out {
        match merged.last_mut() {
            Some((last, kind)) if *kind == token && last.end == range.start => last.end = range.end,
            _ => merged.push((range, token)),
        }
    }
    merged
}

const TS_KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "of",
    "return",
    "switch",
    "throw",
    "try",
    "type",
    "typeof",
    "var",
    "void",
    "while",
    "yield",
];
const TS_CONSTANTS: &[&str] = &[
    "true",
    "false",
    "null",
    "undefined",
    "NaN",
    "Infinity",
    "this",
];
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while",
];
const RUST_CONSTANTS: &[&str] = &["true", "false", "self", "Self", "None", "Some", "Ok", "Err"];

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The end of a quoted run opened at `start` by `quote`: past the closing
/// quote, or the end of the line. A backslash escapes the next character.
fn quoted(line: &str, start: usize, quote: char) -> (usize, bool) {
    let mut escaped = false;
    for (i, c) in line[start..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return (start + i + c.len_utf8(), true);
        }
    }
    (line.len(), false)
}

fn c_like(line: &str, lang: Lang, carry: &mut Carry) -> Vec<(Range<usize>, Token)> {
    let (keywords, constants) = match lang {
        Lang::Rust => (RUST_KEYWORDS, RUST_CONSTANTS),
        _ => (TS_KEYWORDS, TS_CONSTANTS),
    };
    let mut out = Vec::new();
    let mut at = 0;
    if carry.block_comment {
        let end = line.find("*/").map_or(line.len(), |i| i + 2);
        carry.block_comment = end == line.len() && !line.ends_with("*/");
        out.push((0..end, Token::Comment));
        at = end;
    } else if carry.template {
        let (end, closed) = quoted(line, 0, '`');
        carry.template = !closed;
        out.push((0..end, Token::String));
        at = end;
    }
    while at < line.len() {
        let rest = &line[at..];
        let c = rest.chars().next().expect("not at the end");
        if rest.starts_with("//") {
            out.push((at..line.len(), Token::Comment));
            break;
        }
        if let Some(body) = rest.strip_prefix("/*") {
            let end = body.find("*/").map(|i| at + 2 + i + 2);
            carry.block_comment = end.is_none();
            let end = end.unwrap_or(line.len());
            out.push((at..end, Token::Comment));
            at = end;
            continue;
        }
        if c == '`' && lang == Lang::TypeScript {
            let (end, closed) = quoted(line, at + 1, '`');
            carry.template = !closed;
            out.push((at..end, Token::String));
            at = end;
            continue;
        }
        // A Rust quote is a char literal only when it closes within a
        // character or an escape; otherwise it opens a lifetime.
        let rust_char = |rest: &str| {
            let mut chars = rest.chars().skip(1);
            match chars.next() {
                Some('\\') => true,
                Some(_) => chars.next() == Some('\''),
                None => false,
            }
        };
        if c == '"' || (c == '\'' && (lang != Lang::Rust || rust_char(rest))) {
            let (end, _) = quoted(line, at + 1, c);
            out.push((at..end, Token::String));
            at = end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
                .map_or(line.len(), |i| at + i);
            out.push((at..end, Token::Number));
            at = end;
            continue;
        }
        if word_char(c) {
            let end = rest
                .find(|ch: char| !word_char(ch))
                .map_or(line.len(), |i| at + i);
            let word = &line[at..end];
            let next = line[end..].trim_start().chars().next();
            let token = if keywords.contains(&word) {
                Token::Keyword
            } else if constants.contains(&word) {
                Token::Number
            } else if next == Some('(') || (lang == Lang::Rust && next == Some('!')) {
                Token::Call
            } else {
                Token::Plain
            };
            out.push((at..end, token));
            at = end;
            continue;
        }
        at += c.len_utf8();
    }
    out
}

fn shell(line: &str) -> Vec<(Range<usize>, Token)> {
    let mut out = Vec::new();
    let mut at = 0;
    // Whether the next word names the program to run.
    let mut command = true;
    while at < line.len() {
        let rest = &line[at..];
        let c = rest.chars().next().expect("not at the end");
        if c.is_whitespace() {
            at += c.len_utf8();
            continue;
        }
        if c == '#' {
            out.push((at..line.len(), Token::Comment));
            break;
        }
        if matches!(c, '|' | '&' | ';' | '(' | ')') {
            let end = at
                + rest
                    .find(|ch: char| !matches!(ch, '|' | '&' | ';' | '(' | ')'))
                    .unwrap_or(rest.len());
            out.push((at..end, Token::Operator));
            command = true;
            at = end;
            continue;
        }
        if matches!(c, '<' | '>') {
            let end = at
                + rest
                    .find(|ch: char| !matches!(ch, '<' | '>' | '&' | '0'..='9'))
                    .unwrap_or(rest.len());
            out.push((at..end, Token::Operator));
            at = end;
            continue;
        }
        // One word, its quoted parts and variables inside it coloured.
        let start = at;
        let mut parts = Vec::new();
        while at < line.len() {
            let ch = line[at..].chars().next().expect("not at the end");
            if ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '(' | ')' | '<' | '>') {
                break;
            }
            if ch == '\'' || ch == '"' {
                let (end, _) = quoted(line, at + 1, ch);
                parts.push((at..end, Token::String));
                at = end;
            } else if ch == '$' {
                let after = &line[at + 1..];
                let len = if after.starts_with('{') {
                    after.find('}').map_or(after.len(), |i| i + 1)
                } else {
                    after.find(|x: char| !word_char(x)).unwrap_or(after.len())
                };
                parts.push((at..at + 1 + len, Token::Variable));
                at += 1 + len;
            } else {
                at += ch.len_utf8();
            }
        }
        let word = &line[start..at];
        if command
            && let Some(eq) = word
                .find('=')
                .filter(|&i| i > 0 && word[..i].chars().all(word_char))
        {
            // An assignment before the command: `NAME=value cmd`.
            out.push((start..start + eq, Token::Variable));
            out.extend(parts);
            continue;
        }
        let whole = if command {
            command = false;
            Some(Token::Command)
        } else if word.starts_with('-') {
            Some(Token::Flag)
        } else {
            None
        };
        match whole {
            // A quoted program name keeps its quotes' colour.
            Some(token) if parts.is_empty() => out.push((start..at, token)),
            _ => out.extend(parts),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each token's text and kind, for reading a test.
    fn spans(line: &str, lang: Lang) -> Vec<(&str, Token)> {
        let mut carry = Carry::default();
        tokens(line, lang, &mut carry)
            .into_iter()
            .map(|(range, token)| (&line[range], token))
            .collect()
    }

    #[test]
    fn typescript_colours_keywords_strings_calls_numbers_and_comments() {
        use Token::*;
        assert_eq!(
            spans(
                r#"for (const path of await tools.glob({ pattern: "src/**/*.ts" })) {"#,
                Lang::TypeScript
            ),
            [
                ("for", Keyword),
                ("const", Keyword),
                ("of", Keyword),
                ("await", Keyword),
                ("glob", Call),
                (r#""src/**/*.ts""#, String),
            ]
        );
        assert_eq!(
            spans(
                "if (n > 0x1f && ok === true) return null; // done",
                Lang::TypeScript
            ),
            [
                ("if", Keyword),
                ("0x1f", Number),
                ("true", Number),
                ("return", Keyword),
                ("null", Number),
                ("// done", Comment),
            ]
        );
        assert_eq!(
            spans(r#"'it\'s' + `a${b}`"#, Lang::TypeScript),
            [(r#"'it\'s'"#, String), ("`a${b}`", String)]
        );
    }

    #[test]
    fn a_block_comment_or_template_carries_to_the_next_line() {
        let mut carry = Carry::default();
        let lines = [
            "let a = 1; /* open",
            "still a comment",
            "end */ let b = `x",
            "y` + 2",
        ];
        let got: Vec<Vec<(&str, Token)>> = lines
            .iter()
            .map(|line| {
                tokens(line, Lang::TypeScript, &mut carry)
                    .into_iter()
                    .map(|(r, t)| (&line[r], t))
                    .collect()
            })
            .collect();
        use Token::*;
        assert_eq!(
            got[0],
            [("let", Keyword), ("1", Number), ("/* open", Comment)]
        );
        assert_eq!(got[1], [("still a comment", Comment)]);
        assert_eq!(
            got[2],
            [("end */", Comment), ("let", Keyword), ("`x", String)]
        );
        assert_eq!(got[3], [("y`", String), ("2", Number)]);
        assert_eq!(carry, Carry::default());
    }

    #[test]
    fn rust_tells_a_lifetime_from_a_char() {
        use Token::*;
        assert_eq!(
            spans(
                "fn f<'a>(c: char) -> &'a str { if c == 'x' { vec![] } else { None } }",
                Lang::Rust
            ),
            [
                ("fn", Keyword),
                ("if", Keyword),
                ("'x'", String),
                ("vec", Call),
                ("else", Keyword),
                ("None", Number)
            ]
        );
    }

    #[test]
    fn shell_colours_the_command_flags_strings_variables_and_operators() {
        use Token::*;
        assert_eq!(
            spans(
                r#"rg -n "commands.register" -g '*.ts' | head -5"#,
                Lang::Shell
            ),
            [
                ("rg", Command),
                ("-n", Flag),
                (r#""commands.register""#, String),
                ("-g", Flag),
                ("'*.ts'", String),
                ("|", Operator),
                ("head", Command),
                ("-5", Flag),
            ]
        );
        // An assignment before the command, redirects, a second command
        // after `&&`, variables, and a comment.
        assert_eq!(
            spans(
                "CI=1 bun test >out.txt 2>&1 && echo $HOME ${X} # note",
                Lang::Shell
            ),
            [
                ("CI", Variable),
                ("bun", Command),
                (">", Operator),
                (">&1", Operator),
                ("&&", Operator),
                ("echo", Command),
                ("$HOME", Variable),
                ("${X}", Variable),
                ("# note", Comment),
            ]
        );
    }

    #[test]
    fn lexing_never_changes_the_text() {
        let lines = [
            "const s = \"unterminated",
            "echo 'half",
            "/* never closed",
            "日本語 = \"界\" // 注",
            "$",
            "a=",
        ];
        for lang in [Lang::TypeScript, Lang::Rust, Lang::Shell] {
            for line in lines {
                let mut carry = Carry::default();
                let mut last = 0;
                for (range, _) in tokens(line, lang, &mut carry) {
                    assert!(
                        range.start >= last && range.end <= line.len(),
                        "{line:?} {lang:?}"
                    );
                    assert!(line.is_char_boundary(range.start) && line.is_char_boundary(range.end));
                    last = range.end;
                }
            }
        }
    }

    #[test]
    fn files_take_their_language_from_the_extension() {
        assert_eq!(lang_for_path("src/parser.ts"), Some(Lang::TypeScript));
        assert_eq!(lang_for_path("crates/x/src/lib.rs"), Some(Lang::Rust));
        assert_eq!(lang_for_path("scripts/build.sh"), Some(Lang::Shell));
        assert_eq!(lang_for_path("README.md"), None);
        assert_eq!(lang_for_path("Makefile"), None);
        assert_eq!(lang_for_path("dir.ts/file"), None);
    }

    #[test]
    fn every_token_has_a_style_and_none_without_colour() {
        use Token::*;
        let all = [
            Plain, Keyword, String, Number, Comment, Call, Operator, Command, Flag, Variable,
        ];
        for token in all {
            assert_eq!(style(token, Tones::None).fg, None);
        }
        assert_eq!(
            style(String, Tones::TrueColor).fg,
            Some(Color::Rgb(0xbe, 0xf2, 0x64))
        );
        assert!(
            style(Comment, Tones::None)
                .add_modifier
                .contains(Modifier::DIM)
        );
    }
}
