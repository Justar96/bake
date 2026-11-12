//! Mapping of provider-neutral simple options onto request options.
//!
//! Ported from Pi `packages/ai/src/api/simple-options.ts` (v1.1.0; the
//! output clamp's margin from v1.5.2).

use crate::models::clamp_thinking_level;
use crate::options::{SimpleStreamOptions, StreamOptions};
use crate::types::{
    JsonObject, Model, ModelThinkingLevel, ThinkingBudgets, ThinkingLevel, TranscriptContext,
};
use crate::utils::estimate::estimate_context_tokens;

const CONTEXT_SAFETY_TOKENS: u64 = 4096;
/// OpenRouter counts the input with its own tokenizer and rejects input
/// plus `max_tokens` above the window before routing; it counted prose up
/// to 12% above the provider's count (Pi #10497).
const CONTEXT_SAFETY_RATIO: f64 = 0.15;
/// The proportional margin is kept only while it leaves this much output;
/// otherwise only the fixed margin is kept and output is capped at this.
const MIN_OUTPUT_WITH_RATIO_MARGIN: i128 = 32_768;
const MIN_MAX_TOKENS: u64 = 1;

/// Tokens always left for the answer when a thinking budget shares the ceiling.
pub const MIN_ANSWER_TOKENS: u64 = 1024;

/// Default token budgets per thinking level.
pub const DEFAULT_THINKING_BUDGETS: ThinkingBudgets = ThinkingBudgets {
    minimal: Some(1024),
    low: Some(2048),
    medium: Some(8192),
    high: Some(16384),
};

/// Caps `max_tokens` to what the context window leaves after the estimated
/// context and a safety margin, never below 1. The margin is 15% of the
/// estimated input, at least 4096 tokens; when that would leave less than
/// 32768 tokens of output, only 4096 are kept and output is capped at 32768
/// (Pi v1.5.2, #11712).
pub fn clamp_max_tokens_to_context(
    model: &Model,
    context: &TranscriptContext,
    max_tokens: u64,
) -> u64 {
    if model.context_window == 0 {
        return max_tokens.max(MIN_MAX_TOKENS);
    }
    let input = estimate_context_tokens(context.messages()).tokens;
    // `Math.ceil(input * 0.15)` in doubles; the cast saturates.
    let ratio_margin = (input as f64 * CONTEXT_SAFETY_RATIO).ceil() as u64;
    let margin = i128::from(CONTEXT_SAFETY_TOKENS.max(ratio_margin));
    let window = i128::from(model.context_window);
    let input = i128::from(input);
    let mut room = window - input - margin;
    if room < MIN_OUTPUT_WITH_RATIO_MARGIN {
        room = MIN_OUTPUT_WITH_RATIO_MARGIN.min(window - input - i128::from(CONTEXT_SAFETY_TOKENS));
    }
    let room = u64::try_from(room).unwrap_or(0).max(MIN_MAX_TOKENS);
    max_tokens.min(room)
}

/// Model sampling parameters, then those of the effective thinking level,
/// then the request's; `None` when all are absent.
pub fn resolve_sampling_params(
    model: &Model,
    thinking_level: ModelThinkingLevel,
    request_params: Option<&JsonObject>,
) -> Option<JsonObject> {
    let effective = clamp_thinking_level(model, thinking_level);
    let level_params = model
        .sampling_params_by_thinking_level
        .as_ref()
        .and_then(|by_level| by_level.get(effective.as_str()));
    if model.sampling_params.is_none() && level_params.is_none() && request_params.is_none() {
        return None;
    }
    let mut merged = JsonObject::new();
    for source in [model.sampling_params.as_ref(), level_params, request_params]
        .into_iter()
        .flatten()
    {
        for (key, value) in source {
            merged.insert(key.clone(), value.clone());
        }
    }
    Some(merged)
}

/// The base request options for a simple request.
pub fn build_base_options(
    model: &Model,
    context: &TranscriptContext,
    options: &SimpleStreamOptions,
) -> StreamOptions {
    let base = &options.base;
    let reasoning = options
        .reasoning
        .map_or(ModelThinkingLevel::Off, ModelThinkingLevel::from);
    let mut built = base.clone();
    built.sampling_params =
        resolve_sampling_params(model, reasoning, base.sampling_params.as_ref());
    built.max_tokens = Some(clamp_max_tokens_to_context(
        model,
        context,
        base.max_tokens.unwrap_or(model.max_tokens),
    ));
    built
}

/// `xhigh` and `max` clamp to `high` for budget-based providers.
pub fn clamp_reasoning(effort: ThinkingLevel) -> ThinkingLevel {
    match effort {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        other => other,
    }
}

/// The token budget for a thinking level, custom budgets first.
pub fn thinking_budget_for_level(level: ThinkingLevel, custom: Option<&ThinkingBudgets>) -> u64 {
    let pick = |budgets: &ThinkingBudgets| match clamp_reasoning(level) {
        ThinkingLevel::Minimal => budgets.minimal,
        ThinkingLevel::Low => budgets.low,
        ThinkingLevel::Medium => budgets.medium,
        _ => budgets.high,
    };
    custom
        .and_then(pick)
        .or_else(|| pick(&DEFAULT_THINKING_BUDGETS))
        .unwrap_or(0)
}

/// Caps a thinking budget so `MIN_ANSWER_TOKENS` remain under `ceiling`.
pub fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
    thinking_budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}

/// The output limit and thinking budget for budget-based thinking.
/// `base_max_tokens` of `None` means no caller cap.
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    level: ThinkingLevel,
    custom: Option<&ThinkingBudgets>,
) -> (u64, u64) {
    let mut thinking_budget = thinking_budget_for_level(level, custom);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => base.saturating_add(thinking_budget).min(model_max_tokens),
    };
    if max_tokens <= thinking_budget {
        thinking_budget = clamp_thinking_budget_to_answer_room(thinking_budget, max_tokens);
    }
    (max_tokens, thinking_budget)
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `sampling-options.test.ts` and `max-thinking.test.ts`.

    use super::*;
    use crate::providers::faux::faux_model;
    use crate::transcript::normalize_context;
    use crate::types::Context;
    use serde_json::json;

    #[test]
    fn simple_requests_keep_both_transport_timeouts() {
        for idle_timeout_ms in [None, Some(0), Some(120_000)] {
            let options = SimpleStreamOptions::from(StreamOptions {
                timeout_ms: Some(30_000),
                idle_timeout_ms,
                ..Default::default()
            });
            let built =
                build_base_options(&faux_model("m"), &TranscriptContext::default(), &options);
            assert_eq!(built.timeout_ms, Some(30_000));
            assert_eq!(built.idle_timeout_ms, idle_timeout_ms);
        }
    }

    #[test]
    fn merges_sampling_params_in_order() {
        let mut model = faux_model("m");
        assert_eq!(
            resolve_sampling_params(&model, ModelThinkingLevel::Off, None),
            None
        );
        model.sampling_params = Some(
            json!({ "top_p": 0.9, "top_k": 20 })
                .as_object()
                .unwrap()
                .clone(),
        );
        let request = json!({ "top_k": 40 }).as_object().unwrap().clone();
        assert_eq!(
            serde_json::Value::Object(
                resolve_sampling_params(&model, ModelThinkingLevel::Off, Some(&request)).unwrap()
            ),
            json!({ "top_p": 0.9, "top_k": 40 })
        );
    }

    #[test]
    fn budgets_and_clamps() {
        assert_eq!(thinking_budget_for_level(ThinkingLevel::Max, None), 16384);
        assert_eq!(
            adjust_max_tokens_for_thinking(None, 32000, ThinkingLevel::High, None),
            (32000, 16384)
        );
        assert_eq!(
            adjust_max_tokens_for_thinking(Some(1000), 32000, ThinkingLevel::Low, None),
            (3048, 2048)
        );
        assert_eq!(
            adjust_max_tokens_for_thinking(None, 4000, ThinkingLevel::High, None),
            (4000, 2976)
        );
        let model = faux_model("m");
        let context = normalize_context(Context::default());
        assert_eq!(
            clamp_max_tokens_to_context(&model, &context, 500_000),
            128_000 - 4096
        );
    }

    /// Pi v1.5.2 `clamp-max-tokens.test.ts` (#10497, #11712).
    mod clamp_max_tokens {
        use super::*;
        use crate::providers::faux::faux_assistant_message;
        use crate::types::{Message, Usage, UserContent, UserMessage};

        const WINDOW: u64 = 1_048_576;

        fn model_with(max_tokens: u64, context_window: u64) -> Model {
            let mut model = faux_model("m");
            model.context_window = context_window;
            model.max_tokens = max_tokens;
            model
        }

        /// The last assistant usage anchors the estimate, so the estimated
        /// input is exactly `input_tokens`.
        fn context_with_input(input_tokens: u64) -> TranscriptContext {
            let mut assistant = faux_assistant_message("ok");
            assistant.usage = Usage {
                input: input_tokens,
                total_tokens: input_tokens,
                ..Usage::default()
            };
            assistant.timestamp = 2;
            normalize_context(Context {
                system_prompt: None,
                messages: vec![
                    Message::User(UserMessage {
                        content: UserContent::Text("hi".into()),
                        timestamp: 1,
                    }),
                    Message::Assistant(assistant),
                ],
                tools: None,
            })
        }

        fn clamp(max_tokens: u64, window: u64, input: u64) -> u64 {
            clamp_max_tokens_to_context(
                &model_with(max_tokens, window),
                &context_with_input(input),
                max_tokens,
            )
        }

        #[test]
        fn leaves_15_percent_of_the_input_unused_so_an_openrouter_overcount_still_fits() {
            // The failing request of #10497: OpenRouter counted 229,747
            // input tokens where Pi estimated 220,815.
            let max_tokens = clamp(943_718, WINDOW, 220_815);
            assert_eq!(max_tokens, WINDOW - 220_815 - 33_123);
            assert!(229_747 + max_tokens <= WINDOW);
        }

        #[test]
        fn leaves_at_least_4096_tokens_unused_for_small_inputs() {
            assert_eq!(clamp(WINDOW, WINDOW, 10_000), WINDOW - 10_000 - 4096);
        }

        #[test]
        fn keeps_the_model_output_limit_when_it_already_fits() {
            assert_eq!(clamp(64_000, WINDOW, 100_000), 64_000);
        }

        #[test]
        fn sends_up_to_32768_tokens_when_the_proportional_margin_leaves_no_room() {
            assert_eq!(clamp(943_718, WINDOW, 912_000), 32_768);
        }

        #[test]
        fn uses_the_real_room_minus_4096_when_less_than_32768_tokens_fit() {
            assert_eq!(clamp(943_718, WINDOW, 1_030_000), WINDOW - 1_030_000 - 4096);
        }

        #[test]
        fn keeps_only_the_fixed_4096_margin_for_small_windows() {
            assert_eq!(clamp(32_768, 32_768, 20_000), 32_768 - 20_000 - 4096);
        }

        #[test]
        fn sends_1_token_when_the_input_already_exceeds_the_window() {
            assert_eq!(clamp(943_718, WINDOW, 1_050_000), 1);
        }

        proptest::proptest! {
            #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

            // At least 1 and at most the model's limit; above 1, the fixed
            // margin always fits, and the 15% margin does unless the output
            // is at most 32768.
            #[test]
            fn the_clamp_keeps_its_margins(
                max_tokens in 1u64..2_000_000,
                window in 1u64..2_000_000,
                input in 0u64..2_500_000,
            ) {
                let clamped = clamp(max_tokens, window, input);
                proptest::prop_assert!((1..=max_tokens).contains(&clamped));
                if clamped > 1 {
                    proptest::prop_assert!(input + clamped + 4096 <= window);
                    let ratio = (input as f64 * 0.15).ceil() as u64;
                    proptest::prop_assert!(
                        clamped <= 32_768 || input + clamped + ratio <= window
                    );
                }
            }
        }
    }
}
