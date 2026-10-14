//! Listing available models (`--list-models [search]`) and fuzzy matching,
//! ported from Pi `packages/coding-agent/src/cli/list-models.ts` and
//! `packages/tui/src/fuzzy.ts` (v1.2.0, revision
//! `06a466935536f8c9bcacb2d5efc0fc021be1bdff`, unchanged since v1.1.0).

use std::cmp::Ordering;

use bake_ai::{InputModality, Model};

use crate::agent_session::format_no_models_available_message;
use crate::model_registry::ModelRegistry;
use crate::print_mode::{SharedWriter, write_out};
use crate::tools::js::is_whitespace;

/// Result of [`fuzzy_match`], Pi's `FuzzyMatch`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    /// Whether all query characters matched in order.
    pub matches: bool,
    /// Match score (lower is better).
    pub score: f64,
}

fn is_word_boundary_char(ch: char) -> bool {
    is_whitespace(ch) || matches!(ch, '-' | '_' | '.' | '/' | ':')
}

fn match_query(query: &[char], text: &[char]) -> FuzzyMatch {
    if query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }
    if query.len() > text.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let mut query_index = 0usize;
    let mut score = 0.0f64;
    let mut last_match_index: Option<usize> = None;
    let mut consecutive_matches = 0usize;

    while query_index < query.len() {
        let start = last_match_index.map_or(0, |last| last + 1);
        let Some(offset) = text[start..]
            .iter()
            .position(|&ch| ch == query[query_index])
        else {
            break;
        };
        let i = start + offset;
        let is_word_boundary = i == 0 || is_word_boundary_char(text[i - 1]);

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

    if query == text {
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
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();
    let query_chars: Vec<char> = query_lower.chars().collect();
    let text_chars: Vec<char> = text_lower.chars().collect();

    let primary = match_query(&query_chars, &text_chars);
    if primary.matches {
        return primary;
    }

    let Some(swapped) = swapped_alphanumeric_query(&query_lower) else {
        return primary;
    };
    let swapped_chars: Vec<char> = swapped.chars().collect();
    let swapped_match = match_query(&swapped_chars, &text_chars);
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
        .split(|ch: char| is_whitespace(ch) || ch == '/')
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

/// JavaScript's `value.toFixed(1)` for a finite, non-negative `value` below
/// `1e21`. Rust rounds an exact tie to even; `toFixed` picks the larger
/// value. A tenths tie is exactly representable only as an odd number of
/// quarters (`x.25`, `x.75`).
fn js_to_fixed_1(value: f64) -> String {
    let quarters = value * 4.0;
    if quarters.fract() == 0.0 && quarters % 2.0 == 1.0 {
        return format!("{:.1}", (value * 10.0).ceil() / 10.0);
    }
    format!("{value:.1}")
}

/// Pi's `formatTokenCount`: human-readable token counts (`200000` -> `"200K"`,
/// `1000000` -> `"1M"`), with JavaScript number arithmetic and `toFixed(1)`.
pub fn format_token_count(count: u64) -> String {
    let format_scaled = |scaled: f64, suffix: &str| {
        if scaled.fract() == 0.0 {
            format!("{scaled}{suffix}")
        } else {
            format!("{}{suffix}", js_to_fixed_1(scaled))
        }
    };
    if count >= 1_000_000 {
        format_scaled(count as f64 / 1_000_000.0, "M")
    } else if count >= 1_000 {
        format_scaled(count as f64 / 1_000.0, "K")
    } else {
        count.to_string()
    }
}

/// ASCII in ICU root collation order, letters case-folded: whitespace and
/// punctuation, then symbols, digits, and letters.
const ICU_ASCII_ORDER: &str =
    " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";

fn collation_weight(ch: char) -> u32 {
    match ICU_ASCII_ORDER.find(ch) {
        Some(index) => u32::try_from(index).unwrap_or(u32::MAX),
        // Other characters sort after ASCII letters, by code point.
        None => 0x80 + u32::from(ch),
    }
}

/// An approximation of JavaScript's `a.localeCompare(b)` in the root locale,
/// as Pi sorts the models table: ASCII ordered as ICU does and compared
/// case-insensitively, then lowercase before uppercase, then by code point.
fn locale_compare(a: &str, b: &str) -> Ordering {
    let primary = |text: &str| {
        text.chars()
            .flat_map(char::to_lowercase)
            .map(collation_weight)
            .collect::<Vec<_>>()
    };
    primary(a)
        .cmp(&primary(b))
        .then_with(|| {
            a.chars()
                .map(char::is_uppercase)
                .cmp(b.chars().map(char::is_uppercase))
        })
        .then_with(|| a.cmp(b))
}

/// Formats the output of [`list_models`] for a slice of available models.
pub fn format_models_list(
    models: &[Model],
    search_pattern: Option<&str>,
    docs_command: &str,
) -> String {
    if models.is_empty() {
        return format!("{}\n", format_no_models_available_message(docs_command));
    }

    let mut filtered = match search_pattern.filter(|pattern| !pattern.is_empty()) {
        Some(pattern) => fuzzy_filter(models, pattern, |m| format!("{} {}", m.provider, m.id)),
        None => models.to_vec(),
    };

    if filtered.is_empty() {
        let pattern = search_pattern.unwrap_or_default();
        return format!("No models matching \"{pattern}\"\n");
    }

    filtered.sort_by(|a, b| {
        locale_compare(&a.provider, &b.provider).then_with(|| locale_compare(&a.id, &b.id))
    });

    struct Row {
        provider: String,
        model: String,
        context: String,
        max_out: String,
        thinking: &'static str,
        images: &'static str,
    }

    let rows: Vec<Row> = filtered
        .into_iter()
        .map(|m| Row {
            provider: m.provider,
            model: m.id,
            context: format_token_count(m.context_window),
            max_out: format_token_count(m.max_tokens),
            thinking: if m.reasoning { "yes" } else { "no" },
            images: if m.input.contains(&InputModality::Image) {
                "yes"
            } else {
                "no"
            },
        })
        .collect();

    let headers = (
        "provider", "model", "context", "max-out", "thinking", "images",
    );
    let w_provider = rows
        .iter()
        .map(|r| r.provider.len())
        .max()
        .unwrap_or(0)
        .max(headers.0.len());
    let w_model = rows
        .iter()
        .map(|r| r.model.len())
        .max()
        .unwrap_or(0)
        .max(headers.1.len());
    let w_context = rows
        .iter()
        .map(|r| r.context.len())
        .max()
        .unwrap_or(0)
        .max(headers.2.len());
    let w_max_out = rows
        .iter()
        .map(|r| r.max_out.len())
        .max()
        .unwrap_or(0)
        .max(headers.3.len());
    let w_thinking = rows
        .iter()
        .map(|r| r.thinking.len())
        .max()
        .unwrap_or(0)
        .max(headers.4.len());
    let w_images = rows
        .iter()
        .map(|r| r.images.len())
        .max()
        .unwrap_or(0)
        .max(headers.5.len());

    let mut out = format!(
        "{:<w_provider$}  {:<w_model$}  {:<w_context$}  {:<w_max_out$}  {:<w_thinking$}  {:<w_images$}\n",
        headers.0, headers.1, headers.2, headers.3, headers.4, headers.5
    );
    for row in rows {
        out.push_str(&format!(
            "{:<w_provider$}  {:<w_model$}  {:<w_context$}  {:<w_max_out$}  {:<w_thinking$}  {:<w_images$}\n",
            row.provider, row.model, row.context, row.max_out, row.thinking, row.images
        ));
    }
    out
}

/// Pi's `listModels`: writes the available models table (or the no-models /
/// no-match message) to `stdout`.
pub fn list_models(
    registry: &ModelRegistry,
    search_pattern: Option<&str>,
    docs_command: &str,
    stdout: &SharedWriter,
) {
    let available = registry.available();
    let text = format_models_list(&available, search_pattern, docs_command);
    write_out(stdout, &text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_ai::ModelCost;

    fn sample_model(
        provider: &str,
        id: &str,
        context_window: u64,
        max_tokens: u64,
        reasoning: bool,
        images: bool,
    ) -> Model {
        Model {
            id: id.into(),
            name: id.into(),
            api: "openai-responses".into(),
            provider: provider.into(),
            base_url: "https://example.com/v1".into(),
            reasoning,
            input: if images {
                vec![InputModality::Text, InputModality::Image]
            } else {
                vec![InputModality::Text]
            },
            cost: ModelCost::default(),
            context_window,
            max_tokens,
            headers: None,
            compat: None,
            thinking_level_map: None,
            input_limits: None,
            prompt_cache: None,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
        }
    }

    #[test]
    fn formats_token_counts_and_models_table() {
        assert_eq!(format_token_count(500), "500");
        assert_eq!(format_token_count(8_000), "8K");
        assert_eq!(format_token_count(128_500), "128.5K");
        assert_eq!(format_token_count(200_000), "200K");
        assert_eq!(format_token_count(1_000_000), "1M");
        assert_eq!(format_token_count(1_500_000), "1.5M");

        let models = vec![
            sample_model("openai", "gpt-5", 200_000, 32_000, true, true),
            sample_model(
                "anthropic",
                "claude-sonnet-4-5",
                1_000_000,
                64_000,
                true,
                false,
            ),
        ];
        let table = format_models_list(&models, None, "bake-rs docs");
        assert_eq!(
            table,
            concat!(
                "provider   model              context  max-out  thinking  images\n",
                "anthropic  claude-sonnet-4-5  1M       64K      yes       no    \n",
                "openai     gpt-5              200K     32K      yes       yes   \n",
            )
        );

        let filtered = format_models_list(&models, Some("sonnet"), "bake-rs docs");
        assert!(filtered.contains("claude-sonnet-4-5"));
        assert!(!filtered.contains("gpt-5"));

        let none = format_models_list(&models, Some("nonexistent"), "bake-rs docs");
        assert_eq!(none, "No models matching \"nonexistent\"\n");
    }

    #[test]
    fn format_token_count_rounds_like_javascript_to_fixed() {
        // `toFixed(1)` rounds an exact tie up; Rust's `{:.1}` rounds it to even.
        assert_eq!(format_token_count(1_250_000), "1.3M");
        assert_eq!(format_token_count(1_250), "1.3K");
        assert_eq!(format_token_count(128_250), "128.3K");
        assert_eq!(format_token_count(2_750), "2.8K");
        // Not exact ties as doubles: 1.05 is above, 1.15 below.
        assert_eq!(format_token_count(1_050_000), "1.1M");
        assert_eq!(format_token_count(1_150), "1.1K");
        assert_eq!(format_token_count(999_950), "1000.0K");
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(u64::MAX), "18446744073709.6M");
        assert_eq!(format_token_count(9_007_199_254_740_993), "9007199254.7M");
    }

    #[test]
    fn models_table_sorts_like_locale_compare() {
        let models = vec![
            sample_model("openai", "GPT-5", 1_000, 1_000, false, false),
            sample_model("openai", "gpt5", 1_000, 1_000, false, false),
            sample_model("openai", "gpt-4o", 1_000, 1_000, false, false),
            sample_model("openai", "gpt-5", 1_000, 1_000, false, false),
            sample_model("openai", "gpt_5", 1_000, 1_000, false, false),
            sample_model("Anthropic", "claude", 1_000, 1_000, false, false),
            sample_model("openai", "claude", 1_000, 1_000, false, false),
        ];
        let table = format_models_list(&models, None, "bake-rs docs");
        let order: Vec<(&str, &str)> = table
            .lines()
            .skip(1)
            .map(|line| {
                let mut columns = line.split_whitespace();
                (columns.next().unwrap(), columns.next().unwrap())
            })
            .collect();
        // Node: ["claude", "gpt_5", "gpt-4o", "gpt-5", "GPT-5", "gpt5"]
        // sorted with `localeCompare`.
        assert_eq!(
            order,
            [
                ("Anthropic", "claude"),
                ("openai", "claude"),
                ("openai", "gpt_5"),
                ("openai", "gpt-4o"),
                ("openai", "gpt-5"),
                ("openai", "GPT-5"),
                ("openai", "gpt5"),
            ]
        );
        assert_eq!(locale_compare("a", "A"), Ordering::Less);
        assert_eq!(locale_compare("A", "b"), Ordering::Less);
        assert_eq!(locale_compare("a b", "a-b"), Ordering::Less);
        assert_eq!(locale_compare("a.b", "ab"), Ordering::Less);
        assert_eq!(locale_compare("ab", "ab"), Ordering::Equal);
        assert_eq!(locale_compare("é", "z"), Ordering::Greater);
    }

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
