//! Listing available models (`--list-models [search]`), ported from Pi
//! `packages/coding-agent/src/cli/list-models.ts` (v1.2.0, revision
//! `06a466935536f8c9bcacb2d5efc0fc021be1bdff`, unchanged since v1.1.0). Its
//! search is Pi's fuzzy filter ([`crate::session::fuzzy`]).

use std::cmp::Ordering;

use bake_ai::{InputModality, Model};

use crate::agent_session::format_no_models_available_message;
use crate::model_registry::ModelRegistry;
use crate::print_mode::{SharedWriter, write_out};
use crate::session::fuzzy::fuzzy_filter;

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
        let table = format_models_list(&models, None, "bake docs");
        assert_eq!(
            table,
            concat!(
                "provider   model              context  max-out  thinking  images\n",
                "anthropic  claude-sonnet-4-5  1M       64K      yes       no    \n",
                "openai     gpt-5              200K     32K      yes       yes   \n",
            )
        );

        let filtered = format_models_list(&models, Some("sonnet"), "bake docs");
        assert!(filtered.contains("claude-sonnet-4-5"));
        assert!(!filtered.contains("gpt-5"));

        let none = format_models_list(&models, Some("nonexistent"), "bake docs");
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
        let table = format_models_list(&models, None, "bake docs");
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
}
