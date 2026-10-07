//! `@` file mentions: the token at the caret, the text a chosen path puts in
//! the draft, and the file menu's choices.
//!
//! Ports the `@` grammar of `bake-file-reference/grammar` and the file half
//! of the TypeScript `completion.ts`. Discovery is the terminal owner's: the
//! view asks for a query with [`crate::state::Effect::FindFiles`] and lists
//! what comes back for it. Paths only; no file is read.

/// What a discovered path names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    /// Choosing a file finishes the mention.
    File,
    /// Choosing a directory keeps the mention open to descend into it.
    Directory,
}

/// One path relative to the working directory, as discovery returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: String,
    pub kind: PathKind,
}

/// The `@path` or `@"path` token that ends at the caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtToken {
    /// Byte offset of its `@`.
    pub start: usize,
    /// The path text after `@` or `@"`.
    pub query: String,
    /// Whether it opened with `@"`.
    pub quoted: bool,
}

/// The token at the caret: `@"` and the text after it, up to the caret, when
/// that holds no `"`; otherwise `@` and the text after it, when that holds
/// no whitespace. Either must open the draft or follow whitespace, so an
/// address like `me@example.com` is not one.
pub fn active_at_token(draft: &str, caret: usize) -> Option<AtToken> {
    let before = draft.get(..caret)?;
    let opens = |at: usize| {
        before[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
    };
    if let Some(quote) = before.rfind('"')
        && let Some(at) = quote.checked_sub(1)
        && before[at..].starts_with("@\"")
        && opens(at)
    {
        return Some(AtToken {
            start: at,
            query: before[quote + 1..].to_owned(),
            quoted: true,
        });
    }
    let word = before
        .char_indices()
        .rfind(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let query = before[word..].strip_prefix('@')?;
    Some(AtToken {
        start: word,
        query: query.to_owned(),
        quoted: false,
    })
}

/// The mention a chosen path puts in the draft: `@path`, or `@"path"` when
/// it holds whitespace or the token opened with a quote. A directory ends in
/// `/` and leaves a quote open to descend further. `None` for a path the
/// grammar cannot carry: one with a control character or a `"`.
pub fn format(candidate: &Candidate, keep_quote: bool) -> Option<String> {
    let directory = candidate.kind == PathKind::Directory;
    let path = if directory {
        format!("{}/", candidate.path)
    } else {
        candidate.path.clone()
    };
    if path.chars().any(|c| c.is_control() || c == '"') {
        return None;
    }
    Some(if !keep_quote && !path.contains(char::is_whitespace) {
        format!("@{path}")
    } else if directory {
        format!("@\"{path}")
    } else {
        format!("@\"{path}\"")
    })
}

/// One row of the file menu and the draft it makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// The mention, as the menu lists it.
    pub name: String,
    pub kind: PathKind,
    pub draft: String,
    pub caret: usize,
}

/// Where the token at the caret ends in the draft, once the caret is inside
/// one: a quoted token at its closing quote, else at the next newline, and a
/// plain one at the next whitespace. `None` past a closing quote, where the
/// mention is finished.
pub fn token_end(draft: &str, caret: usize, token: &AtToken) -> Option<usize> {
    let quoted = draft[token.start..].starts_with("@\"");
    let closing = quoted
        .then(|| draft[token.start + 2..].find('"'))
        .flatten()
        .map(|at| token.start + 2 + at);
    if closing.is_some_and(|closing| caret > closing) {
        return None;
    }
    let rest = &draft[caret..];
    let boundary = if quoted {
        rest.find('\n')
    } else {
        rest.find(char::is_whitespace)
    };
    Some(closing.map_or_else(|| boundary.map_or(draft.len(), |b| caret + b), |c| c + 1))
}

/// The menu's rows for `found`, the paths discovered for the token's query:
/// each formatted mention and the draft it makes. Paths the grammar cannot
/// carry are left out.
pub fn choices(draft: &str, token: &AtToken, end: usize, found: &[Candidate]) -> Vec<Choice> {
    found
        .iter()
        .filter_map(|candidate| {
            let name = format(candidate, token.quoted)?;
            let directory = candidate.kind == PathKind::Directory;
            let (draft, caret) = replace(draft, token.start, end, &name, directory);
            Some(Choice {
                name,
                kind: candidate.kind,
                draft,
                caret,
            })
        })
        .collect()
}

/// The draft with `start..end` replaced by `mention`. A directory keeps the
/// caret at its `/`, closing a quote after it; a file is followed by a space
/// unless whitespace already follows, and the caret goes past it.
fn replace(
    draft: &str,
    start: usize,
    end: usize,
    mention: &str,
    directory: bool,
) -> (String, usize) {
    let prefix = format!("{}{mention}", &draft[..start]);
    let suffix = &draft[end..];
    if directory {
        let close = if mention.starts_with("@\"") { "\"" } else { "" };
        return (format!("{prefix}{close}{suffix}"), prefix.len());
    }
    let separator = if suffix.starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let step = suffix
        .chars()
        .next()
        .filter(|c| c.is_whitespace())
        .map_or(1, char::len_utf8);
    (format!("{prefix}{separator}{suffix}"), prefix.len() + step)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(draft: &str) -> Option<(usize, String, bool)> {
        active_at_token(draft, draft.len()).map(|t| (t.start, t.query, t.quoted))
    }

    fn file(path: &str) -> Candidate {
        Candidate {
            path: path.into(),
            kind: PathKind::File,
        }
    }

    fn dir(path: &str) -> Candidate {
        Candidate {
            path: path.into(),
            kind: PathKind::Directory,
        }
    }

    #[test]
    fn an_at_token_opens_the_draft_or_follows_whitespace() {
        assert_eq!(token("@"), Some((0, "".into(), false)));
        assert_eq!(token("read @src/ma"), Some((5, "src/ma".into(), false)));
        assert_eq!(token("read\n@x"), Some((5, "x".into(), false)));
        assert_eq!(
            token("see @\"my docs/no"),
            Some((4, "my docs/no".into(), true))
        );
        // An address, a finished word, and plain text are not tokens.
        assert_eq!(token("mail me@example.com"), None);
        assert_eq!(token("@src done"), None);
        assert_eq!(token("plain"), None);
        // A closed quote reads as a plain token, which the menu then closes,
        // or, holding a space, as none.
        assert_eq!(token("@\"ab\""), Some((0, "\"ab\"".into(), false)));
        assert_eq!(token("@\"a b\""), None);
        // Only the draft before the caret counts.
        assert_eq!(
            active_at_token("@src more", 4).map(|t| t.query),
            Some("src".into())
        );
        // Multi-byte text before the token keeps byte offsets.
        assert_eq!(token("über @é"), Some((6, "é".into(), false)));
    }

    #[test]
    fn a_path_formats_as_the_grammar_carries_it() {
        assert_eq!(format(&file("src/main.rs"), false).unwrap(), "@src/main.rs");
        assert_eq!(format(&dir("src"), false).unwrap(), "@src/");
        assert_eq!(
            format(&file("my docs/a.md"), false).unwrap(),
            "@\"my docs/a.md\""
        );
        assert_eq!(format(&dir("my docs"), false).unwrap(), "@\"my docs/");
        // An opened quote is kept even when not needed.
        assert_eq!(format(&file("a.md"), true).unwrap(), "@\"a.md\"");
        assert_eq!(format(&file("say\"hi"), false), None);
        assert_eq!(format(&file("line\nbreak"), false), None);
    }

    #[test]
    fn a_choice_replaces_the_whole_token() {
        let draft = "read @sr and";
        let at = active_at_token(draft, 7).unwrap();
        let end = token_end(draft, 7, &at).unwrap();
        assert_eq!(end, 8);
        let made = choices(draft, &at, end, &[file("src/main.rs"), dir("src")]);
        // A file gets the existing space and the caret after it.
        assert_eq!(made[0].draft, "read @src/main.rs and");
        assert_eq!(made[0].caret, "read @src/main.rs ".len());
        // A directory leaves the caret on its slash to keep completing.
        assert_eq!(made[1].draft, "read @src/ and");
        assert_eq!(made[1].caret, "read @src/".len());
        // At the end of the draft a file adds its space.
        let at = active_at_token("@a", 2).unwrap();
        let made = choices("@a", &at, token_end("@a", 2, &at).unwrap(), &[file("a.md")]);
        assert_eq!((made[0].draft.as_str(), made[0].caret), ("@a.md ", 6));
    }

    #[test]
    fn a_quoted_token_ends_at_its_quote_and_closes_after_it() {
        let draft = "@\"my d\" next";
        let at = active_at_token(draft, 5).unwrap();
        assert!(at.quoted);
        assert_eq!(token_end(draft, 5, &at), Some(7));
        let made = choices(draft, &at, 7, &[dir("my docs"), file("my docs.md")]);
        assert_eq!(made[0].draft, "@\"my docs/\" next");
        assert_eq!(made[0].caret, "@\"my docs/".len());
        assert_eq!(made[1].draft, "@\"my docs.md\" next");
        // Past the closing quote the mention is finished.
        let draft = "@\"ab\"";
        let at = active_at_token(draft, draft.len()).unwrap();
        assert_eq!(token_end(draft, draft.len(), &at), None);
    }
}
