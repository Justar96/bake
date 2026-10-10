//! Cost and thinking-level helpers from Pi's model registry.
//!
//! Ported from Pi `packages/ai/src/models.ts` (v1.1.0): `calculateCost`,
//! `getSupportedThinkingLevels`, and `clampThinkingLevel`. The catalog and
//! provider registry are not ported; see [`crate::stream`] for dispatch.

use crate::types::{Model, ModelCostRates, ModelThinkingLevel, Usage, UsageCost};

const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// Sets and returns `usage.cost` from the model's rates; the highest tier
/// whose threshold the total input exceeds applies to the whole request, and
/// 1h cache writes cost twice the input rate.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> UsageCost {
    let input_tokens = usage.input_sum();
    let mut rates: ModelCostRates = model.cost.rates;
    let mut matched: Option<u64> = None;
    for tier in model.cost.tiers.iter().flatten() {
        if input_tokens > tier.input_tokens_above
            && matched.is_none_or(|threshold| tier.input_tokens_above > threshold)
        {
            rates = tier.rates;
            matched = Some(tier.input_tokens_above);
        }
    }
    let long_write = usage.cache_write_1h.unwrap_or(0) as f64;
    let short_write = usage.cache_write as f64 - long_write;
    let cost = &mut usage.cost;
    cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    cost.cache_write =
        (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    cost.total = cost.input + cost.output + cost.cache_read + cost.cache_write;
    *cost
}

/// The thinking levels a model supports, `off` first.
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }
    EXTENDED_THINKING_LEVELS
        .into_iter()
        .filter(|level| match model.thinking_level_value(*level) {
            Some(None) => false,
            mapped => {
                !matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max)
                    || mapped.is_some()
            }
        })
        .collect()
}

/// The nearest supported level, preferring higher levels.
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    let requested = EXTENDED_THINKING_LEVELS
        .iter()
        .position(|known| *known == level)
        .unwrap_or(0);
    let higher = EXTENDED_THINKING_LEVELS
        .iter()
        .skip(requested)
        .find(|candidate| available.contains(candidate));
    let lower = EXTENDED_THINKING_LEVELS
        .iter()
        .take(requested)
        .rev()
        .find(|candidate| available.contains(candidate));
    higher
        .or(lower)
        .copied()
        .or_else(|| available.first().copied())
        .unwrap_or(ModelThinkingLevel::Off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::faux::faux_model;
    use crate::types::{ModelCostTier, ThinkingLevelMap};

    #[test]
    fn calculates_tiered_and_long_write_costs() {
        // Cases from Pi `model-cost-tiers.test.ts` and `anthropic-cache-write-1h-cost.test.ts`.
        let mut model = faux_model("m");
        model.cost.rates = ModelCostRates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        };
        model.cost.tiers = Some(vec![ModelCostTier {
            rates: ModelCostRates {
                input: 6.0,
                output: 22.5,
                cache_read: 0.6,
                cache_write: 7.5,
            },
            input_tokens_above: 200_000,
        }]);
        let mut usage = Usage {
            input: 1_000_000,
            output: 1_000_000,
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        assert_eq!((cost.input, cost.output), (6.0, 22.5));
        let mut usage = Usage {
            input: 100,
            cache_write: 1_000_000,
            cache_write_1h: Some(400_000),
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        // The tier applies: 7.5 for short writes, twice its 6.0 input rate for 1h writes.
        assert!((cost.cache_write - (7.5 * 0.6 + 6.0 * 2.0 * 0.4)).abs() < 1e-9);
    }

    #[test]
    fn clamps_thinking_levels() {
        let mut model = faux_model("m");
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![ModelThinkingLevel::Off]
        );
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::High),
            ModelThinkingLevel::Off
        );
        model.reasoning = true;
        assert_eq!(get_supported_thinking_levels(&model).len(), 5);
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Xhigh),
            ModelThinkingLevel::High
        );
        let mut map = ThinkingLevelMap::default();
        map.0.insert("xhigh".into(), Some("xhigh".into()));
        map.0.insert("minimal".into(), None);
        model.thinking_level_map = Some(map);
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Xhigh),
            ModelThinkingLevel::Xhigh
        );
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Minimal),
            ModelThinkingLevel::Low
        );
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Max),
            ModelThinkingLevel::Xhigh
        );
    }
}
