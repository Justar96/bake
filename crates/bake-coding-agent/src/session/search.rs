//! Session search for the resume picker.
//!
//! Ported from Pi `packages/coding-agent/src/modes/interactive/components/session-selector-search.ts`
//! (v1.2.0, revision `06a466935536f8c9bcacb2d5efc0fc021be1bdff`, MIT; see the
//! crate's `NOTICE`), with its tests from
//! `packages/coding-agent/test/session-selector-search.test.ts`. Fuzzy
//! tokens use the crate's port of Pi's `fuzzyMatch`
//! ([`crate::cli::list_models::fuzzy_match`]).
//!
//! A session's search text is Pi's: its id, name, the text of every user
//! and assistant message in its file ([`SessionInfo::all_messages_text`],
//! every branch, as Pi's listing reads the whole log), and its working
//! directory, joined by spaces. A query is one of:
//!
//! - words, each matched fuzzily, all of which must match;
//! - `"quoted phrases"`, matched as substrings after lowercasing and
//!   collapsing whitespace runs to one space, mixed freely with words; a
//!   query with an unclosed quote falls back to plain words, quotes kept;
//! - `re:<pattern>`, a case-insensitive regular expression.
//!
//! # Deviations from Pi
//!
//! - **Regular expressions** use the `regex` crate's syntax, which matches
//!   in linear time, instead of JavaScript's. Look-around and
//!   backreferences are refused with the crate's reason; `\w`, `\d`, and
//!   `\b` are Unicode-aware where JavaScript's are ASCII-only without the
//!   `u` flag; `\p{…}` classes beyond those are not compiled in. A pattern
//!   is compiled once per query. A pattern longer than
//!   [`MAX_REGEX_PATTERN_BYTES`] is refused before it is parsed, and one
//!   whose compiled program exceeds [`REGEX_SIZE_LIMIT`] is refused as too
//!   large; the lazy DFA's cache is held to [`REGEX_DFA_CACHE_LIMIT`]
//!   separately, which bounds the memory a search uses, not which patterns
//!   compile.
//! - **Scores** count positions in `char`s where Pi counts UTF-16 code
//!   units, so relevance differs from Pi's only for text with characters
//!   beyond the Basic Multilingual Plane.
//! - **Sort modes.** Pi's threaded mode is not offered; with no query,
//!   both modes keep the listing's order, newest first.
//! - **Errors** are returned with the (empty) result so a picker can say
//!   why nothing matches, where Pi only returns no sessions.
//! - **Prepared text.** [`SessionSearchIndex`] lowercases each session's
//!   text once, so typing a query does not lowercase every session again.

use regex::{Regex, RegexBuilder};

use crate::cli::list_models::fuzzy_match_lowercase;
use crate::session::json::{is_js_whitespace, js_trim};
use crate::session::list::SessionInfo;

/// The longest `re:` pattern compiled, in bytes after `re:` and trimming;
/// a longer one is refused before parsing, so its parse allocates nothing.
pub const MAX_REGEX_PATTERN_BYTES: usize = 8 * 1024;

/// The most memory a compiled `re:` program may take, in bytes
/// (`RegexBuilder::size_limit`); a pattern needing more is refused.
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// The capacity of a compiled pattern's lazy DFA cache, in bytes
/// (`RegexBuilder::dfa_size_limit`), apart from [`REGEX_SIZE_LIMIT`]. A
/// search that outgrows it falls back to slower matching, still linear.
pub const REGEX_DFA_CACHE_LIMIT: usize = 1 << 20;

/// Pi's `SortMode`, without `threaded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// The listing's order (newest first), filtered.
    #[default]
    Recent,
    /// Best match first, ties newest first.
    Relevance,
}

/// Pi's `NameFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameFilter {
    #[default]
    All,
    /// Only sessions with a non-blank name.
    Named,
}

/// One token of a word query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchToken {
    Fuzzy(String),
    Phrase(String),
}

/// Why a `re:` query cannot be searched; Pi's `error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// `re:` with nothing after it: Pi's `Empty regex`.
    EmptyRegex,
    /// The pattern was refused, and why, on one line.
    InvalidRegex(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyRegex => f.write_str("Empty regex"),
            Self::InvalidRegex(reason) => f.write_str(reason),
        }
    }
}

/// Pi's `ParsedSearchQuery`.
#[derive(Debug, Clone)]
pub enum ParsedSearchQuery {
    /// Words and phrases, all of which must match; none matches everything.
    Tokens(Vec<SearchToken>),
    /// A compiled `re:` pattern.
    Regex(Regex),
    /// A `re:` query that cannot be searched.
    Invalid(QueryError),
}

/// Pi's `MatchResult`; `None` is no match.
pub type MatchScore = Option<f64>;

/// Pi's `hasSessionName`.
pub fn has_session_name(session: &SessionInfo) -> bool {
    session
        .name
        .as_deref()
        .is_some_and(|name| !js_trim(name).is_empty())
}

/// Pi's `getSessionSearchText`.
pub fn session_search_text(session: &SessionInfo) -> String {
    format!(
        "{} {} {} {}",
        session.id,
        session.name.as_deref().unwrap_or(""),
        session.all_messages_text,
        session.cwd
    )
}

/// Pi's `normalizeWhitespaceLower` of text already lowercased.
fn collapse_whitespace(lower: &str) -> String {
    let mut out = String::with_capacity(lower.len());
    for word in lower
        .split(is_js_whitespace)
        .filter(|word| !word.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// The reason a pattern failed to compile, on one line: the `regex` crate's
/// last line, without its `error: ` prefix.
fn regex_reason(error: &regex::Error) -> String {
    match error {
        regex::Error::CompiledTooBig(_) => "pattern is too large".to_owned(),
        other => {
            let text = other.to_string();
            let last = text
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("invalid pattern");
            last.trim()
                .strip_prefix("error: ")
                .unwrap_or(last.trim())
                .to_owned()
        }
    }
}

/// Pi's `parseSearchQuery`.
pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = js_trim(query);
    if trimmed.is_empty() {
        return ParsedSearchQuery::Tokens(Vec::new());
    }

    if let Some(pattern) = trimmed.strip_prefix("re:") {
        let pattern = js_trim(pattern);
        if pattern.is_empty() {
            return ParsedSearchQuery::Invalid(QueryError::EmptyRegex);
        }
        if pattern.len() > MAX_REGEX_PATTERN_BYTES {
            return ParsedSearchQuery::Invalid(QueryError::InvalidRegex(format!(
                "pattern is longer than {MAX_REGEX_PATTERN_BYTES} bytes"
            )));
        }
        return match RegexBuilder::new(pattern)
            .case_insensitive(true)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_DFA_CACHE_LIMIT)
            .build()
        {
            Ok(regex) => ParsedSearchQuery::Regex(regex),
            Err(error) => {
                ParsedSearchQuery::Invalid(QueryError::InvalidRegex(regex_reason(&error)))
            }
        };
    }

    let mut tokens = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    let mut flush = |buf: &mut String, phrase: bool| {
        let value = js_trim(buf).to_owned();
        buf.clear();
        if !value.is_empty() {
            tokens.push(if phrase {
                SearchToken::Phrase(value)
            } else {
                SearchToken::Fuzzy(value)
            });
        }
    };
    for ch in trimmed.chars() {
        if ch == '"' {
            flush(&mut buf, in_quote);
            in_quote = !in_quote;
            continue;
        }
        if !in_quote && is_js_whitespace(ch) {
            flush(&mut buf, false);
            continue;
        }
        buf.push(ch);
    }

    // Unbalanced quotes: plain whitespace-separated words, quotes kept.
    if in_quote {
        return ParsedSearchQuery::Tokens(
            trimmed
                .split(is_js_whitespace)
                .filter(|word| !word.is_empty())
                .map(|word| SearchToken::Fuzzy(word.to_owned()))
                .collect(),
        );
    }
    flush(&mut buf, false);
    ParsedSearchQuery::Tokens(tokens)
}

/// A session's search text, prepared once.
#[derive(Clone)]
struct Prepared {
    text: String,
    lower: String,
    named: bool,
    modified_ms: i64,
}

impl Prepared {
    fn new(session: &SessionInfo) -> Self {
        let text = session_search_text(session);
        Self {
            lower: text.to_lowercase(),
            text,
            named: has_session_name(session),
            modified_ms: session.modified_ms,
        }
    }

    /// Pi's `matchSession`.
    fn score(&self, parsed: &ParsedSearchQuery) -> MatchScore {
        match parsed {
            ParsedSearchQuery::Invalid(_) => None,
            ParsedSearchQuery::Regex(regex) => {
                let found = regex.find(&self.text)?;
                Some(self.text[..found.start()].chars().count() as f64 * 0.1)
            }
            ParsedSearchQuery::Tokens(tokens) => {
                let mut total = 0.0;
                let mut normalized: Option<String> = None;
                for token in tokens {
                    match token {
                        SearchToken::Phrase(phrase) => {
                            let normalized =
                                normalized.get_or_insert_with(|| collapse_whitespace(&self.lower));
                            let phrase = collapse_whitespace(&phrase.to_lowercase());
                            if phrase.is_empty() {
                                continue;
                            }
                            let at = normalized.find(&phrase)?;
                            total += normalized[..at].chars().count() as f64 * 0.1;
                        }
                        SearchToken::Fuzzy(word) => {
                            let found = fuzzy_match_lowercase(&word.to_lowercase(), &self.lower);
                            if !found.matches {
                                return None;
                            }
                            total += found.score;
                        }
                    }
                }
                Some(total)
            }
        }
    }
}

/// What a search found: indexes into the sessions searched, in order, and
/// why the query cannot match when it cannot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchOutcome {
    pub indexes: Vec<usize>,
    pub error: Option<QueryError>,
}

/// Sessions prepared for repeated searches, as a picker searches on every
/// keystroke. `Debug` shows only how many sessions it holds.
#[derive(Clone)]
pub struct SessionSearchIndex {
    sessions: Vec<Prepared>,
}

impl std::fmt::Debug for SessionSearchIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionSearchIndex(<{} sessions>)", self.sessions.len())
    }
}

impl SessionSearchIndex {
    /// Prepares `sessions`, in the order searches report them.
    pub fn new(sessions: &[SessionInfo]) -> Self {
        Self {
            sessions: sessions.iter().map(Prepared::new).collect(),
        }
    }

    /// Sessions held.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether it holds no session.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Pi's `filterAndSortSessions`, as indexes: the name filter first,
    /// then the query, once parsed. Each index appears at most once.
    pub fn search(&self, query: &str, sort: SortMode, name: NameFilter) -> SearchOutcome {
        let candidates = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| name == NameFilter::All || session.named);
        if js_trim(query).is_empty() {
            return SearchOutcome {
                indexes: candidates.map(|(index, _)| index).collect(),
                error: None,
            };
        }
        let parsed = parse_search_query(query);
        if let ParsedSearchQuery::Invalid(reason) = parsed {
            return SearchOutcome {
                indexes: Vec::new(),
                error: Some(reason),
            };
        }
        let mut scored: Vec<(usize, f64)> = candidates
            .filter_map(|(index, session)| session.score(&parsed).map(|score| (index, score)))
            .collect();
        if sort == SortMode::Relevance {
            // Stable, as `Array.prototype.sort` is.
            scored.sort_by(|a, b| {
                a.1.total_cmp(&b.1).then_with(|| {
                    self.sessions[b.0]
                        .modified_ms
                        .cmp(&self.sessions[a.0].modified_ms)
                })
            });
        }
        SearchOutcome {
            indexes: scored.into_iter().map(|(index, _)| index).collect(),
            error: None,
        }
    }
}

/// Pi's `matchSession`: `None` when `session` does not match.
pub fn match_session(session: &SessionInfo, parsed: &ParsedSearchQuery) -> MatchScore {
    Prepared::new(session).score(parsed)
}

/// Pi's `filterAndSortSessions`.
pub fn filter_and_sort_sessions<'a>(
    sessions: &'a [SessionInfo],
    query: &str,
    sort: SortMode,
    name: NameFilter,
) -> Vec<&'a SessionInfo> {
    SessionSearchIndex::new(sessions)
        .search(query, sort, name)
        .indexes
        .into_iter()
        .map(|index| &sessions[index])
        .collect()
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
