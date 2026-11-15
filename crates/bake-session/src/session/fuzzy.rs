//! Pi's fuzzy matching, ported from `packages/tui/src/fuzzy.ts` (v1.2.0,
//! revision `06a466935536f8c9bcacb2d5efc0fc021be1bdff`, unchanged since
//! v1.1.0), with the cases of `packages/tui/test/fuzzy.test.ts`. Session
//! search ([`super::search`]) and the model list use it.

use super::json::is_js_whitespace;

/// Result of [`fuzzy_match`], Pi's `FuzzyMatch`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    /// Whether all query characters matched in order.
    pub matches: bool,
    /// Match score (lower is better).
    pub score: f64,
}

fn is_word_boundary_char(ch: char) -> bool {
    is_js_whitespace(ch) || matches!(ch, '-' | '_' | '.' | '/' | ':')
}

/// Pi's inner `matchQuery` over a lowercased `text`. Indexes count `char`s
/// where Pi counts UTF-16 code units, so only text with characters beyond
/// the Basic Multilingual Plane scores differently.
fn match_query(query: &[char], text: &str) -> FuzzyMatch {
    if query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }
    if query.len() > text.chars().count() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let mut query_index = 0usize;
    let mut score = 0.0f64;
    let mut last_match_index: Option<usize> = None;
    let mut consecutive_matches = 0usize;
    // Scanning resumes after the last match, as Pi's `indexOf(…, last + 1)`.
    let mut chars = text.chars().enumerate();
    let mut previous: Option<char> = None;

    while query_index < query.len() {
        let wanted = query[query_index];
        let mut found = None;
        for (i, ch) in chars.by_ref() {
            let before = previous.replace(ch);
            if ch == wanted {
                found = Some((i, before));
                break;
            }
        }
        let Some((i, before)) = found else {
            break;
        };
        let is_word_boundary = before.is_none_or(is_word_boundary_char);

        // Pi's `lastMatchIndex === i - 1` with `lastMatchIndex` starting at
        // -1: a first match at index 0 also counts as consecutive.
        let is_consecutive = last_match_index.map_or(i == 0, |last| last + 1 == i);
        if is_consecutive {
            consecutive_matches += 1;
            score -= (consecutive_matches as f64) * 5.0;
        } else {
            consecutive_matches = 0;
            if let Some(last) = last_match_index {
                score += ((i - last - 1) as f64) * 2.0;
            }
        }

        if is_word_boundary {
            score -= 10.0;
        }

        score += (i as f64) * 0.1;

        last_match_index = Some(i);
        query_index += 1;
    }

    if query_index < query.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    if text.chars().eq(query.iter().copied()) {
        score -= 100.0;
    }

    FuzzyMatch {
        matches: true,
        score,
    }
}

fn swapped_alphanumeric_query(query_lower: &str) -> Option<String> {
    let bytes = query_lower.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    if bytes[0].is_ascii_lowercase() {
        let letters = bytes.iter().take_while(|b| b.is_ascii_lowercase()).count();
        let digits = bytes[letters..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if letters > 0 && digits > 0 && letters + digits == bytes.len() {
            return Some(format!(
                "{}{}",
                &query_lower[letters..],
                &query_lower[..letters]
            ));
        }
    } else if bytes[0].is_ascii_digit() {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        let letters = bytes[digits..]
            .iter()
            .take_while(|b| b.is_ascii_lowercase())
            .count();
        if digits > 0 && letters > 0 && digits + letters == bytes.len() {
            return Some(format!(
                "{}{}",
                &query_lower[digits..],
                &query_lower[..digits]
            ));
        }
    }
    None
}

/// Pi's `fuzzyMatch`: matches if all query characters appear in order
/// (case-insensitive), with a fallback for swapped `letters+digits` /
/// `digits+letters` queries. Lower score is a better match.
pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    fuzzy_match_lowercase(&query.to_lowercase(), &text.to_lowercase())
}

/// [`fuzzy_match`] of a query and a text both already lowercased, so a
/// caller that matches one text many times lowercases it once.
pub(crate) fn fuzzy_match_lowercase(query_lower: &str, text_lower: &str) -> FuzzyMatch {
    let query_chars: Vec<char> = query_lower.chars().collect();

    let primary = match_query(&query_chars, text_lower);
    if primary.matches {
        return primary;
    }

    let Some(swapped) = swapped_alphanumeric_query(query_lower) else {
        return primary;
    };
    let swapped_chars: Vec<char> = swapped.chars().collect();
    let swapped_match = match_query(&swapped_chars, text_lower);
    if !swapped_match.matches {
        return primary;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

/// Pi's `fuzzyFilter`: filter and sort items by fuzzy match quality (best
/// matches first). Supports whitespace- and slash-separated tokens, all of
/// which must match.
pub fn fuzzy_filter<T: Clone>(items: &[T], query: &str, get_text: impl Fn(&T) -> String) -> Vec<T> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return items.to_vec();
    }

    let tokens: Vec<&str> = trimmed
        .split(|ch: char| is_js_whitespace(ch) || ch == '/')
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return items.to_vec();
    }

    let mut results: Vec<(usize, f64, T)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let text = get_text(item);
        let mut total_score = 0.0;
        let mut all_match = true;
        for token in &tokens {
            let m = fuzzy_match(token, &text);
            if m.matches {
                total_score += m.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            results.push((idx, total_score, item.clone()));
        }
    }

    results.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    results.into_iter().map(|(_, _, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_match_counts_a_first_match_at_index_zero_as_consecutive() {
        // Pi: `c` at 0 is consecutive to -1 (-5) and a boundary (-10); `o` at
        // 9 adds a gap of 8 (+16) and 0.9.
        let start = fuzzy_match("co", "claude groq");
        assert!(start.matches);
        assert!((start.score - 1.9).abs() < 1e-9, "{start:?}");
        let exact = fuzzy_match("test", "test");
        assert!((exact.score - -159.4).abs() < 1e-9, "{exact:?}");
        assert_eq!(
            fuzzy_filter(&["anthropic codex", "claude groq"], "co", |s| (*s)
                .to_owned()),
            ["claude groq", "anthropic codex"]
        );
    }

    // Bake: the streaming `matchQuery` over a lowercased `&str` keeps the
    // scores of the earlier `&[char]` one, which these values were taken
    // from: Unicode lowercasing, characters outside the Basic Multilingual
    // Plane counted as one, and word boundaries.
    #[test]
    fn match_query_keeps_its_scores_for_unicode_text() {
        let cases: [(&str, &str, Option<f64>); 13] = [
            ("über", "ÉCOLE ÜBER straße", Some(-37.0)),
            ("ÉC", "école", Some(-24.9)),
            ("ss", "straße", None),
            ("İ", "İstanbul", Some(-24.9)),
            ("ab", "a😀b", Some(-12.8)),
            ("😀b", "x 😀-b", Some(-17.4)),
            ("codex52", "gpt-5.2-codex", None),
            ("", "anything", Some(0.0)),
            ("zz", "abc", None),
            ("abc", "abc", Some(-139.7)),
            ("fb", "foo-bar", Some(-18.6)),
            ("foo", "f_o_o_bar", Some(-30.4)),
            ("longquery", "short", None),
        ];
        for (query, text, score) in cases {
            let query: Vec<char> = query.to_lowercase().chars().collect();
            let found = match_query(&query, &text.to_lowercase());
            assert_eq!(found.matches, score.is_some(), "{query:?} in {text:?}");
            if let Some(score) = score {
                assert!(
                    (found.score - score).abs() < 1e-9,
                    "{query:?} in {text:?}: {}",
                    found.score
                );
            }
        }
        // The prelowered entry point keeps the swapped-digits fallback.
        assert!(fuzzy_match_lowercase("codex52", "gpt-5.2-codex").matches);
    }

    // Ported from Pi `packages/tui/test/fuzzy.test.ts` (`describe("fuzzyMatch")`).
    #[test]
    fn fuzzy_match_follows_pi() {
        // "empty query matches everything with score 0"
        assert_eq!(
            fuzzy_match("", "anything"),
            FuzzyMatch {
                matches: true,
                score: 0.0
            }
        );
        // "query longer than text does not match"
        assert!(!fuzzy_match("longquery", "short").matches);
        // "exact match has good score"
        let exact = fuzzy_match("test", "test");
        assert!(exact.matches);
        assert!(exact.score < 0.0);
        // "characters must appear in order"
        assert!(fuzzy_match("abc", "aXbXc").matches);
        assert!(!fuzzy_match("abc", "cba").matches);
        // "case insensitive matching"
        assert!(fuzzy_match("ABC", "abc").matches);
        assert!(fuzzy_match("abc", "ABC").matches);
        // "consecutive matches score better than scattered matches"
        let consecutive = fuzzy_match("foo", "foobar");
        let scattered = fuzzy_match("foo", "f_o_o_bar");
        assert!(consecutive.matches && scattered.matches);
        assert!(consecutive.score < scattered.score);
        // "word boundary matches score better"
        let at_boundary = fuzzy_match("fb", "foo-bar");
        let not_at_boundary = fuzzy_match("fb", "afbx");
        assert!(at_boundary.matches && not_at_boundary.matches);
        assert!(at_boundary.score < not_at_boundary.score);
        // "matches swapped alpha numeric tokens"
        assert!(fuzzy_match("codex52", "gpt-5.2-codex").matches);
    }

    // Ported from Pi `packages/tui/test/fuzzy.test.ts` (`describe("fuzzyFilter")`).
    #[test]
    fn fuzzy_filter_follows_pi() {
        let text = |s: &&str| (*s).to_owned();
        // "empty query returns all items unchanged"
        let fruits = ["apple", "banana", "cherry"];
        assert_eq!(fuzzy_filter(&fruits, "", text), fruits);
        // "filters out non-matching items"
        assert_eq!(fuzzy_filter(&fruits, "an", text), ["banana"]);
        // "sorts results by match quality"
        let apps = ["a_p_p", "app", "application"];
        assert_eq!(fuzzy_filter(&apps, "app", text)[0], "app");
        // "prioritizes exact matches over longer prefix matches"
        assert_eq!(fuzzy_filter(&["clone", "cl"], "cl", text), ["cl", "clone"]);
        // "works with custom getText function"
        let items = [("foo", 1), ("bar", 2), ("foobar", 3)];
        let named = fuzzy_filter(&items, "foo", |item| item.0.to_owned());
        assert_eq!(named.len(), 2);
        assert!(named.iter().any(|item| item.0 == "foo"));
        assert!(named.iter().any(|item| item.0 == "foobar"));
        // "matches slash-separated provider/model queries against reordered text"
        let model = [("gpt-5.5", "openai-codex")];
        assert_eq!(
            fuzzy_filter(&model, "openai-codex/gpt-5.5", |m| format!(
                "{} {}",
                m.0, m.1
            )),
            model
        );
    }
}
