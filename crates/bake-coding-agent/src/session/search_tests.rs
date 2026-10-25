//! Pi `test/session-selector-search.test.ts` (v1.2.0), ported, then Bake's
//! cases for what that file leaves implicit: words, unbalanced quotes,
//! Unicode, regex limits and errors, and stable relevance ties.

use std::path::PathBuf;

use super::*;

fn session(id: &str, modified: &str, text: &str) -> SessionInfo {
    named(id, None, modified, text)
}

fn named(id: &str, name: Option<&str>, modified: &str, text: &str) -> SessionInfo {
    SessionInfo {
        path: PathBuf::from(format!("/tmp/{id}.jsonl")),
        id: id.to_owned(),
        cwd: String::new(),
        name: name.map(str::to_owned),
        parent_session_path: None,
        created_ms: Some(0),
        modified_ms: crate::session::time::parse_date_ms(modified).expect("date"),
        message_count: 1,
        first_message: "(no messages)".to_owned(),
        all_messages_text: text.to_owned(),
    }
}

fn ids(sessions: &[SessionInfo], query: &str, sort: SortMode, name: NameFilter) -> Vec<String> {
    filter_and_sort_sessions(sessions, query, sort, name)
        .into_iter()
        .map(|session| session.id.clone())
        .collect()
}

fn recent(sessions: &[SessionInfo], query: &str) -> Vec<String> {
    ids(sessions, query, SortMode::Recent, NameFilter::All)
}

#[test]
fn filters_by_quoted_phrase_with_whitespace_normalization() {
    let sessions = [
        session(
            "a",
            "2026-01-01T00:00:00.000Z",
            "node\n\n   cve was discussed",
        ),
        session("b", "2026-01-02T00:00:00.000Z", "node something else"),
    ];
    assert_eq!(recent(&sessions, "\"node cve\""), ["a"]);
}

#[test]
fn filters_by_regex_and_is_case_insensitive() {
    let sessions = [
        session("a", "2026-01-02T00:00:00.000Z", "Brave is great"),
        session("b", "2026-01-03T00:00:00.000Z", "bravery is not the same"),
    ];
    assert_eq!(recent(&sessions, "re:\\bbrave\\b"), ["a"]);
}

#[test]
fn recent_sort_preserves_input_order() {
    let sessions = [
        session("newer", "2026-01-03T00:00:00.000Z", "brave"),
        session("older", "2026-01-01T00:00:00.000Z", "brave"),
        session("nomatch", "2026-01-04T00:00:00.000Z", "something else"),
    ];
    assert_eq!(recent(&sessions, "\"brave\""), ["newer", "older"]);
}

#[test]
fn relevance_sort_orders_by_score_and_tie_breaks_by_modified_desc() {
    let sessions = [
        session("late", "2026-01-03T00:00:00.000Z", "xxxx brave"),
        session("early", "2026-01-01T00:00:00.000Z", "brave xxxx"),
    ];
    assert_eq!(
        ids(&sessions, "\"brave\"", SortMode::Relevance, NameFilter::All),
        ["early", "late"]
    );
    let ties = [
        session("older", "2026-01-01T00:00:00.000Z", "brave"),
        session("newer", "2026-01-03T00:00:00.000Z", "brave"),
    ];
    assert_eq!(
        ids(&ties, "\"brave\"", SortMode::Relevance, NameFilter::All),
        ["newer", "older"]
    );
}

#[test]
fn returns_empty_list_for_invalid_regex() {
    let sessions = [session("a", "2026-01-01T00:00:00.000Z", "brave")];
    assert!(recent(&sessions, "re:(").is_empty());
}

fn name_sessions() -> Vec<SessionInfo> {
    vec![
        named(
            "named1",
            Some("My Project"),
            "2026-01-03T00:00:00.000Z",
            "blueberry",
        ),
        named(
            "named2",
            Some("Another Named"),
            "2026-01-02T00:00:00.000Z",
            "blueberry",
        ),
        session("other1", "2026-01-04T00:00:00.000Z", "blueberry"),
        session("other2", "2026-01-01T00:00:00.000Z", "blueberry"),
    ]
}

#[test]
fn name_filter_all_returns_every_session() {
    assert_eq!(
        ids(&name_sessions(), "", SortMode::Recent, NameFilter::All),
        ["named1", "named2", "other1", "other2"]
    );
}

#[test]
fn name_filter_named_returns_only_named_sessions() {
    assert_eq!(
        ids(&name_sessions(), "", SortMode::Recent, NameFilter::Named),
        ["named1", "named2"]
    );
}

#[test]
fn name_filter_applies_before_the_query() {
    assert_eq!(
        ids(
            &name_sessions(),
            "blueberry",
            SortMode::Recent,
            NameFilter::Named
        ),
        ["named1", "named2"]
    );
}

#[test]
fn name_filter_excludes_whitespace_only_names() {
    let sessions = [
        named(
            "whitespace",
            Some("   "),
            "2026-01-01T00:00:00.000Z",
            "test",
        ),
        named("empty", Some(""), "2026-01-02T00:00:00.000Z", "test"),
        named(
            "named",
            Some("Real Name"),
            "2026-01-03T00:00:00.000Z",
            "test",
        ),
    ];
    assert_eq!(
        ids(&sessions, "", SortMode::Recent, NameFilter::Named),
        ["named"]
    );
}

// Bake: Pi's parser, case by case.
#[test]
fn parses_words_phrases_and_unbalanced_quotes() {
    let tokens = |query: &str| match parse_search_query(query) {
        ParsedSearchQuery::Tokens(tokens) => tokens,
        other => panic!("{other:?}"),
    };
    use SearchToken::{Fuzzy, Phrase};
    assert_eq!(
        tokens("  foo \"node  cve\" bar "),
        [
            Fuzzy("foo".into()),
            Phrase("node  cve".into()),
            Fuzzy("bar".into())
        ]
    );
    // A quote ends the word before it.
    assert_eq!(
        tokens("ab\"cd\"ef"),
        [Fuzzy("ab".into()), Phrase("cd".into()), Fuzzy("ef".into())]
    );
    // An empty phrase is dropped.
    assert_eq!(tokens("\"\" x"), [Fuzzy("x".into())]);
    // Unbalanced: plain words with their quotes.
    assert_eq!(
        tokens("\"node cve"),
        [Fuzzy("\"node".into()), Fuzzy("cve".into())]
    );
    assert!(tokens("   ").is_empty());
}

// Bake: an unclosed quote still searches, by fuzzy words that keep the
// quote character, which therefore must appear in the text.
#[test]
fn an_unclosed_quote_falls_back_to_words() {
    let sessions = [
        session("quoted", "2026-01-02T00:00:00.000Z", "say \"node then cve"),
        session("plain", "2026-01-01T00:00:00.000Z", "node cve"),
    ];
    assert_eq!(recent(&sessions, "\"node cve"), ["quoted"]);
}

// Bake: every word must match fuzzily, in order within the word, anywhere
// in the id, name, messages, or directory.
#[test]
fn fuzzy_words_all_match_across_fields() {
    let mut a = named(
        "0190aaaa",
        Some("Release"),
        "2026-01-02T00:00:00.000Z",
        "draft the changelog",
    );
    a.cwd = "/work/bake".into();
    let b = session("0190bbbb", "2026-01-01T00:00:00.000Z", "release notes");
    let sessions = [a, b];
    assert_eq!(recent(&sessions, "chglog wrk"), ["0190aaaa"]);
    assert_eq!(recent(&sessions, "RELEASE"), ["0190aaaa", "0190bbbb"]);
    assert_eq!(recent(&sessions, "bbbb"), ["0190bbbb"]);
    assert!(recent(&sessions, "zzz").is_empty());
}

// Bake: lowercasing and whitespace are Unicode-aware, as JavaScript's
// `toLowerCase` and `\s` are.
#[test]
fn unicode_text_and_whitespace() {
    let sessions = [
        session("a", "2026-01-02T00:00:00.000Z", "ÉCOLE\u{3000}ÜBER straße"),
        session("b", "2026-01-01T00:00:00.000Z", "ecole uber"),
    ];
    assert_eq!(recent(&sessions, "\"école über\""), ["a"]);
    assert_eq!(recent(&sessions, "über"), ["a"]);
    assert_eq!(recent(&sessions, "re:STRASSE|straße"), ["a"]);
    assert_eq!(recent(&sessions, "re:ÉCOLE"), ["a"]);
}

// Bake: errors say why, and no session matches.
#[test]
fn invalid_and_empty_patterns_report_why() {
    let index = SessionSearchIndex::new(&[session("a", "2026-01-01T00:00:00.000Z", "x")]);
    let outcome = index.search("re:", SortMode::Recent, NameFilter::All);
    assert_eq!(outcome.error, Some(QueryError::EmptyRegex));
    assert_eq!(QueryError::EmptyRegex.to_string(), "Empty regex");
    assert!(outcome.indexes.is_empty());
    let outcome = index.search("re:   ", SortMode::Relevance, NameFilter::All);
    assert_eq!(outcome.error, Some(QueryError::EmptyRegex));
    let outcome = index.search("re:(", SortMode::Recent, NameFilter::All);
    let reason = outcome.error.expect("error").to_string();
    assert!(reason.contains("unclosed group"), "{reason}");
    assert!(!reason.contains('\n'));
    // JavaScript features the `regex` crate refuses.
    for pattern in ["re:a(?=b)", "re:(a)\\1"] {
        let outcome = index.search(pattern, SortMode::Recent, NameFilter::All);
        assert!(outcome.error.is_some(), "{pattern}");
        assert!(outcome.indexes.is_empty());
    }
    // A pattern too large to compile within the bound.
    let outcome = index.search("re:(\\w{100}){100}", SortMode::Recent, NameFilter::All);
    assert_eq!(
        outcome.error,
        Some(QueryError::InvalidRegex("pattern is too large".into()))
    );
    // A pattern past the byte bound is refused whole, never cut short;
    // one at the bound compiles.
    let at_bound = format!("re:{}", "a".repeat(MAX_REGEX_PATTERN_BYTES));
    assert_eq!(
        index
            .search(&at_bound, SortMode::Recent, NameFilter::All)
            .error,
        None
    );
    let past = format!("re:{}", "a".repeat(MAX_REGEX_PATTERN_BYTES + 1));
    let outcome = index.search(&past, SortMode::Recent, NameFilter::All);
    assert_eq!(
        outcome.error,
        Some(QueryError::InvalidRegex(
            "pattern is longer than 8192 bytes".into()
        ))
    );
    assert!(outcome.indexes.is_empty());
    // An empty query is not an error, and `re` alone is a word.
    assert_eq!(
        index.search("  ", SortMode::Recent, NameFilter::All),
        SearchOutcome {
            indexes: vec![0],
            error: None
        }
    );
    assert_eq!(
        index.search("re", SortMode::Recent, NameFilter::All).error,
        None
    );
}

// Bake: relevance ranks earlier regex matches first; equal scores and equal
// times keep the listing's order.
#[test]
fn relevance_is_stable_and_scores_regex_position() {
    let sessions = [
        session("far", "2026-01-02T00:00:00.000Z", "xxxxxxxxxx needle"),
        session("near", "2026-01-01T00:00:00.000Z", "needle"),
        session("tie1", "2026-01-01T00:00:00.000Z", "needle"),
    ];
    assert_eq!(
        ids(&sessions, "re:needle", SortMode::Relevance, NameFilter::All),
        ["near", "tie1", "far"]
    );
    assert_eq!(recent(&sessions, "re:needle"), ["far", "near", "tie1"]);
}

// Bake: the index answers each search with distinct, in-range indexes.
#[test]
fn indexes_are_distinct_and_in_range() {
    let sessions: Vec<SessionInfo> = (0..50)
        .map(|n| {
            session(
                &format!("s{n:02}"),
                "2026-01-01T00:00:00.000Z",
                &format!("text {n}"),
            )
        })
        .collect();
    let index = SessionSearchIndex::new(&sessions);
    for query in ["", "text", "\"text 1\"", "re:\\d", "t 4"] {
        for sort in [SortMode::Recent, SortMode::Relevance] {
            let found = index.search(query, sort, NameFilter::All).indexes;
            let mut seen = found.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), found.len(), "{query}");
            assert!(found.iter().all(|&i| i < sessions.len()));
        }
    }
    assert_eq!(format!("{index:?}"), "SessionSearchIndex(<50 sessions>)");
}
