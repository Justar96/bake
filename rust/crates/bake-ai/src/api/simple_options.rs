//! Mapping of provider-neutral simple options onto request options.
//!
//! Ported from Pi `packages/ai/src/api/simple-options.ts` (v1.1.0).

use crate::models::clamp_thinking_level;
use crate::options::{SimpleStreamOptions, StreamOptions};
use crate::types::{
    JsonObject, Model, ModelThinkingLevel, ThinkingBudgets, ThinkingLevel, TranscriptContext,
};
use crate::utils::estimate::estimate_context_tokens;

const CONTEXT_SAFETY_TOKENS: u64 = 4096;
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
/// context and a safety margin, never below 1.
pub fn clamp_max_tokens_to_context(
    model: &Model,
    context: &TranscriptContext,
    max_tokens: u64,
) -> u64 {
    if model.context_window == 0 {
        return max_tokens.max(MIN_MAX_TOKENS);
    }
    let used = estimate_context_tokens(context.messages())
        .tokens
        .saturating_add(CONTEXT_SAFETY_TOKENS);
    let available = model.context_window.saturating_sub(used);
    max_tokens.min(available.max(MIN_MAX_TOKENS))
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
}
