//! The matcher both hook dialects share, ported from Bake 0.3's
//! `packages/hooks/hook-protocol/src/matcher.ts`.
//!
//! Claude Code reads a pattern of letters, digits, `_`, and `|` as literal
//! alternatives and any other pattern as a regex; Codex reads every
//! non-empty pattern as an unanchored regex. A missing or empty pattern, or
//! `*`, matches everything. At run time an invalid regex is a non-match;
//! the config parsers reject it first through [`matcher_diagnostic`].
//!
//! Regexes use the `regex` crate's syntax, where 0.3 used JavaScript's:
//! look-around and backreferences are refused as invalid.

use regex::Regex;

use super::HookDialect;

/// Whether `matcher` is a match-all sentinel: absent, empty, or `*`.
fn is_match_all(matcher: Option<&str>) -> bool {
    matches!(matcher, None | Some("" | "*"))
}

/// Whether `pattern` is a Claude Code literal: word characters and `|`.
fn is_claude_literal(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'|')
}

/// Why `matcher` is invalid for `dialect`, or `None` when it is valid.
pub fn matcher_diagnostic(matcher: Option<&str>, dialect: HookDialect) -> Option<String> {
    let pattern = matcher.filter(|_| !is_match_all(matcher))?;
    if dialect == HookDialect::ClaudeCode && is_claude_literal(pattern) {
        return None;
    }
    Regex::new(pattern).err().map(|_| {
        format!(
            "invalid {} regex matcher {}",
            dialect.as_str(),
            serde_json::Value::from(pattern)
        )
    })
}

/// Whether `matcher` selects `query` under `dialect`: Claude Code literals
/// match one pipe-separated alternative exactly; other patterns are
/// unanchored regexes, and an invalid one matches nothing.
pub fn matches_matcher(matcher: Option<&str>, query: &str, dialect: HookDialect) -> bool {
    let Some(pattern) = matcher.filter(|_| !is_match_all(matcher)) else {
        return true;
    };
    if dialect == HookDialect::ClaudeCode && is_claude_literal(pattern) {
        return pattern.split('|').any(|alternative| alternative == query);
    }
    Regex::new(pattern).is_ok_and(|regex| regex.is_match(query))
}

#[cfg(test)]
mod tests {
    //! Ported from 0.3's `hook-protocol/tests/matcher.spec.ts`.
    use super::*;

    const CLAUDE: HookDialect = HookDialect::ClaudeCode;
    const CODEX: HookDialect = HookDialect::Codex;

    #[test]
    fn absent_empty_and_star_match_everything_in_both_dialects() {
        for dialect in [CLAUDE, CODEX] {
            assert!(matches_matcher(None, "Bash", dialect));
            assert!(matches_matcher(Some(""), "anything", dialect));
            assert!(matches_matcher(Some("*"), "whatever", dialect));
        }
    }

    #[test]
    fn a_claude_word_pattern_is_an_exact_literal() {
        assert!(matches_matcher(Some("Bash"), "Bash", CLAUDE));
        assert!(!matches_matcher(Some("Bash"), "BashOutput", CLAUDE));
    }

    #[test]
    fn a_claude_pipe_pattern_is_literal_alternation() {
        assert!(matches_matcher(Some("Edit|Write"), "Edit", CLAUDE));
        assert!(matches_matcher(Some("Edit|Write"), "Write", CLAUDE));
        assert!(!matches_matcher(Some("Edit|Write"), "Read", CLAUDE));
        assert!(!matches_matcher(Some("Edit|Write"), "EditFile", CLAUDE));
    }

    #[test]
    fn a_claude_non_word_pattern_is_an_unanchored_regex() {
        assert!(matches_matcher(Some("^Bash$"), "Bash", CLAUDE));
        assert!(matches_matcher(Some("Bash.*"), "BashOutput", CLAUDE));
        assert!(matches_matcher(Some(r".*\.ts$"), "foo.ts", CLAUDE));
        assert!(!matches_matcher(Some(r".*\.ts$"), "foo.js", CLAUDE));
    }

    #[test]
    fn a_codex_word_pattern_is_an_unanchored_regex() {
        assert!(matches_matcher(Some("Bash"), "Bash", CODEX));
        assert!(matches_matcher(Some("Bash"), "BashOutput", CODEX));
        assert!(matches_matcher(Some("Edit|Write"), "Edit", CODEX));
        assert!(matches_matcher(Some("^Bash$"), "Bash", CODEX));
        assert!(!matches_matcher(Some("^Bash$"), "BashOutput", CODEX));
    }

    #[test]
    fn an_invalid_regex_matches_nothing() {
        assert!(!matches_matcher(Some("("), "x", CLAUDE));
        assert!(!matches_matcher(Some("["), "x", CODEX));
    }

    #[test]
    fn diagnostics_accept_sentinels_literals_and_valid_regexes() {
        assert_eq!(matcher_diagnostic(None, CLAUDE), None);
        assert_eq!(matcher_diagnostic(Some(""), CODEX), None);
        assert_eq!(matcher_diagnostic(Some("*"), CODEX), None);
        assert_eq!(matcher_diagnostic(Some("Edit|Write"), CLAUDE), None);
        assert_eq!(matcher_diagnostic(Some("^Bash$"), CLAUDE), None);
        assert_eq!(matcher_diagnostic(Some("Edit|Write"), CODEX), None);
    }

    #[test]
    fn diagnostics_name_an_invalid_regex_and_its_dialect() {
        assert_eq!(
            matcher_diagnostic(Some("("), CLAUDE).as_deref(),
            Some(r#"invalid claude-code regex matcher "(""#)
        );
        assert_eq!(
            matcher_diagnostic(Some("["), CODEX).as_deref(),
            Some(r#"invalid codex regex matcher "[""#)
        );
    }
}
