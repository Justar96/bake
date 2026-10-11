//! Model patterns from the command line and the initial model.
//!
//! Ported from Pi `packages/coding-agent/src/core/model-resolver.ts`
//! (v1.1.0): `findExactModelReferenceMatch`, `parseModelPattern`,
//! `resolveCliModel`, and `findInitialModel`, with Pi's
//! `defaultModelPerProvider` table. `--models` scoping
//! (`resolveModelScope`) is not ported.
//!
//! # Deviations from Pi
//!
//! Pi orders candidate ids with `String.prototype.localeCompare`, which
//! uses ICU collation. [`locale_compare`] approximates it for model ids:
//! case-insensitive, punctuation before digits before letters, then
//! lowercase before uppercase.

use std::cmp::Ordering;

use bake_ai::{Model, ModelThinkingLevel};

use super::ModelRegistry;

/// What the resolver reads of a registry; [`ModelRegistry`] implements it.
pub trait ModelCatalog {
    /// Every model.
    fn all_models(&self) -> Vec<Model>;
    /// Whether the provider has configured auth.
    fn provider_has_auth(&self, provider: &str) -> bool;
}

impl ModelCatalog for ModelRegistry {
    fn all_models(&self) -> Vec<Model> {
        self.models()
    }

    fn provider_has_auth(&self, provider: &str) -> bool {
        self.has_configured_auth(provider)
    }
}

/// Pi's `DEFAULT_THINKING_LEVEL`.
pub const DEFAULT_THINKING_LEVEL: ModelThinkingLevel = ModelThinkingLevel::Medium;

/// Pi's `defaultModelPerProvider`, in Pi's order.
pub const DEFAULT_MODEL_PER_PROVIDER: &[(&str, &str)] = &[
    ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
    ("ant-ling", "Ring-2.6-1T"),
    ("anthropic", "claude-opus-4-8"),
    ("openai", "gpt-5.5"),
    ("azure", "gpt-5.4"),
    ("openai-codex", "gpt-6.1-sol"),
    ("radius", "balanced"),
    ("nvidia", "nvidia/nemotron-3-ultra-550b-a55b"),
    ("deepseek", "deepseek-v4-pro"),
    ("google", "gemini-3.1-pro-preview"),
    ("google-vertex", "gemini-3.1-pro-preview"),
    ("github-copilot", "gpt-5.4"),
    ("openrouter", "moonshotai/kimi-k2.6"),
    ("vercel-ai-gateway", "zai/glm-5.1"),
    ("xai", "grok-4.7"),
    ("groq", "openai/gpt-oss-120b"),
    ("cerebras", "gpt-oss-120b"),
    ("zai", "glm-5.3"),
    ("zai-coding-cn", "glm-5.3"),
    ("mistral", "devstral-medium-latest"),
    ("minimax", "MiniMax-M2.7"),
    ("minimax-cn", "MiniMax-M2.7"),
    ("moonshotai", "kimi-k2.6"),
    ("moonshotai-cn", "kimi-k2.6"),
    ("huggingface", "moonshotai/Kimi-K2.6"),
    ("fireworks", "accounts/fireworks/models/kimi-k3"),
    ("together", "moonshotai/Kimi-K3"),
    ("baseten", "zai-org/GLM-5.2"),
    ("opencode", "kimi-k2.6"),
    ("opencode-go", "kimi-k3"),
    ("kimi-coding", "kimi-for-coding"),
    ("meta", "muse-spark-1.3"),
    ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
    (
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    ),
    ("qwen-token-plan", "qwen3.7-max"),
    ("qwen-token-plan-cn", "qwen3.7-max"),
    ("qwen-token-plan-individual", "qwen3.8-max"),
    ("xiaomi", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-cn", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-ams", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-sgp", "mimo-v2.5-pro"),
];

/// Pi's `isValidThinkingLevel`.
pub fn parse_thinking_level(level: &str) -> Option<ModelThinkingLevel> {
    Some(match level {
        "off" => ModelThinkingLevel::Off,
        "minimal" => ModelThinkingLevel::Minimal,
        "low" => ModelThinkingLevel::Low,
        "medium" => ModelThinkingLevel::Medium,
        "high" => ModelThinkingLevel::High,
        "xhigh" => ModelThinkingLevel::Xhigh,
        "max" => ModelThinkingLevel::Max,
        _ => return None,
    })
}

fn collation_class(ch: char) -> u8 {
    if ch.is_alphabetic() {
        2
    } else if ch.is_numeric() {
        1
    } else {
        0
    }
}

/// An approximation of `a.localeCompare(b)` for model ids.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let primary = |text: &str| -> Vec<(u8, char)> {
        text.chars()
            .flat_map(char::to_lowercase)
            .map(|ch| (collation_class(ch), ch))
            .collect()
    };
    primary(a).cmp(&primary(b)).then_with(|| {
        // Lowercase sorts before uppercase at the tertiary level.
        let tertiary = |text: &str| -> Vec<bool> { text.chars().map(char::is_uppercase).collect() };
        tertiary(a).cmp(&tertiary(b))
    })
}

/// Pi's `isAlias`: ids ending in `-latest` or without a `-YYYYMMDD` date.
fn is_alias(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }
    let bytes = id.as_bytes();
    let dated = bytes.len() >= 9
        && bytes[bytes.len() - 9] == b'-'
        && bytes[bytes.len() - 8..].iter().all(u8::is_ascii_digit);
    !dated
}

fn models_equal(a: &Model, b: &Model) -> bool {
    a.provider == b.provider && a.id == b.id
}

/// Pi's `findExactModelReferenceMatch`: a bare id or `provider/id`, case
/// insensitively; ambiguous matches are no match.
pub fn find_exact_model_reference_match<'a>(
    reference: &str,
    models: &'a [Model],
) -> Option<&'a Model> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_lowercase();
    let canonical: Vec<&Model> = models
        .iter()
        .filter(|model| format!("{}/{}", model.provider, model.id).to_lowercase() == normalized)
        .collect();
    match canonical.as_slice() {
        [one] => return Some(one),
        [] => {}
        _ => return None,
    }
    if let Some(slash) = trimmed.find('/') {
        let provider = trimmed[..slash].trim().to_lowercase();
        let id = trimmed[slash + 1..].trim().to_lowercase();
        if !provider.is_empty() && !id.is_empty() {
            let matches: Vec<&Model> = models
                .iter()
                .filter(|model| {
                    model.provider.to_lowercase() == provider && model.id.to_lowercase() == id
                })
                .collect();
            match matches.as_slice() {
                [one] => return Some(one),
                [] => {}
                _ => return None,
            }
        }
    }
    let ids: Vec<&Model> = models
        .iter()
        .filter(|model| model.id.to_lowercase() == normalized)
        .collect();
    match ids.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

/// Pi's `tryMatchModel`: an exact reference, else a partial id or name
/// match preferring aliases, then the highest id.
fn try_match_model(pattern: &str, models: &[Model]) -> Option<Model> {
    if let Some(exact) = find_exact_model_reference_match(pattern, models) {
        return Some(exact.clone());
    }
    let lower = pattern.to_lowercase();
    let matches: Vec<&Model> = models
        .iter()
        .filter(|model| {
            model.id.to_lowercase().contains(&lower) || model.name.to_lowercase().contains(&lower)
        })
        .collect();
    if matches.is_empty() {
        return None;
    }
    let (mut aliases, mut dated): (Vec<&Model>, Vec<&Model>) =
        matches.into_iter().partition(|model| is_alias(&model.id));
    let pick = |list: &mut Vec<&Model>| {
        // A stable sort, descending, as Pi sorts with `b.localeCompare(a)`.
        list.sort_by(|a, b| locale_compare(&b.id, &a.id));
        list.first().map(|model| (*model).clone())
    };
    if aliases.is_empty() {
        pick(&mut dated)
    } else {
        pick(&mut aliases)
    }
}

/// Pi's `ParsedModelResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedModel {
    /// The model.
    pub model: Option<Model>,
    /// A thinking level given as a `:<level>` suffix.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// A warning for an invalid suffix.
    pub warning: Option<String>,
}

/// Pi's `parseModelPattern`: the whole pattern, else the part before the
/// last colon with the suffix as a thinking level. With
/// `allow_invalid_thinking_level_fallback`, an invalid suffix is dropped
/// with a warning; without it, the pattern does not match.
pub fn parse_model_pattern(
    pattern: &str,
    models: &[Model],
    allow_invalid_thinking_level_fallback: bool,
) -> ParsedModel {
    let mut suffixes: Vec<&str> = Vec::new();
    let mut prefix = pattern;
    // Iterative form of Pi's recursion on the prefix.
    let model = loop {
        if let Some(model) = try_match_model(prefix, models) {
            break Some(model);
        }
        let Some(colon) = prefix.rfind(':') else {
            break None;
        };
        let suffix = &prefix[colon + 1..];
        if parse_thinking_level(suffix).is_none() && !allow_invalid_thinking_level_fallback {
            break None;
        }
        suffixes.push(suffix);
        prefix = &prefix[..colon];
    };
    let Some(model) = model else {
        return ParsedModel {
            model: None,
            thinking_level: None,
            warning: None,
        };
    };
    // Unwind innermost first, as Pi's recursion returns.
    let mut thinking_level = None;
    let mut warning: Option<String> = None;
    let mut current = prefix.to_owned();
    for suffix in suffixes.iter().rev() {
        current = format!("{current}:{suffix}");
        match parse_thinking_level(suffix) {
            Some(level) => {
                thinking_level = if warning.is_some() { None } else { Some(level) };
            }
            None => {
                thinking_level = None;
                warning = Some(format!(
                    "Invalid thinking level \"{suffix}\" in pattern \"{current}\". Using default instead."
                ));
            }
        }
    }
    ParsedModel {
        model: Some(model),
        thinking_level,
        warning,
    }
}

/// Pi's `ResolveCliModelResult`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CliModel {
    /// The model.
    pub model: Option<Model>,
    /// A thinking level from a `:<level>` suffix.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// A warning to print.
    pub warning: Option<String>,
    /// An error to print; then `model` is `None`.
    pub error: Option<String>,
}

fn found(model: Model) -> CliModel {
    CliModel {
        model: Some(model),
        ..CliModel::default()
    }
}

/// Pi's `buildFallbackModel`: the provider's default (or first) model under
/// a custom id.
fn build_fallback_model(provider: &str, id: &str, models: &[Model]) -> Option<Model> {
    let provider_models: Vec<&Model> = models
        .iter()
        .filter(|model| model.provider == provider)
        .collect();
    let first = provider_models.first()?;
    let default_id = DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .find(|(known, _)| *known == provider)
        .map(|(_, id)| *id);
    let base = default_id
        .and_then(|default_id| provider_models.iter().find(|model| model.id == default_id))
        .unwrap_or(first);
    let mut model = (*base).clone();
    model.id = id.to_owned();
    model.name = id.to_owned();
    Some(model)
}

/// Pi's `resolveCliModel`: `--provider <name> --model <pattern>`, or
/// `--model <provider>/<pattern>`, with an optional `:<thinking>` suffix,
/// matched against every model (not only those with auth).
pub fn resolve_cli_model(
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
    cli_thinking: Option<ModelThinkingLevel>,
    registry: &impl ModelCatalog,
) -> CliModel {
    let Some(cli_model) = cli_model else {
        return CliModel::default();
    };
    let models = registry.all_models();
    if models.is_empty() {
        return CliModel {
            error: Some(
                "No models available. Check your installation or add models to models.json."
                    .to_owned(),
            ),
            ..CliModel::default()
        };
    }
    let canonical_provider = |name: &str| {
        let lower = name.to_lowercase();
        // The last model of a provider spelling wins, as Pi's `Map.set`.
        models
            .iter()
            .rev()
            .find(|model| model.provider.to_lowercase() == lower)
            .map(|model| model.provider.clone())
    };
    let mut provider = cli_provider.and_then(canonical_provider);
    if let Some(cli_provider) = cli_provider
        && provider.is_none()
    {
        return CliModel {
            error: Some(format!(
                "Unknown provider \"{cli_provider}\". Use --list-models to see available providers/models."
            )),
            ..CliModel::default()
        };
    }
    let mut pattern = cli_model.to_owned();
    let mut inferred_provider = false;
    if provider.is_none()
        && let Some(slash) = cli_model.find('/')
        && let Some(canonical) = canonical_provider(&cli_model[..slash])
    {
        provider = Some(canonical);
        pattern = cli_model[slash + 1..].to_owned();
        inferred_provider = true;
    }
    let lower = cli_model.to_lowercase();
    let is_exact = |model: &Model| {
        model.id.to_lowercase() == lower
            || format!("{}/{}", model.provider, model.id).to_lowercase() == lower
    };
    if provider.is_none() {
        let exact: Vec<&Model> = models.iter().filter(|model| is_exact(model)).collect();
        if let [one] = exact.as_slice() {
            return found((*one).clone());
        }
        if exact.len() > 1 {
            let authenticated: Vec<&&Model> = exact
                .iter()
                .filter(|model| registry.provider_has_auth(&model.provider))
                .collect();
            if let [one] = authenticated.as_slice() {
                return found((**one).clone());
            }
            let mut names: Vec<String> = exact
                .iter()
                .map(|model| format!("{}/{}", model.provider, model.id))
                .collect();
            names.sort_by(|a, b| locale_compare(a, b));
            let hint = if authenticated.is_empty() {
                "No matching provider is authenticated."
            } else {
                "More than one matching provider is authenticated."
            };
            return CliModel {
                error: Some(format!(
                    "Model \"{cli_model}\" is ambiguous across providers: {}. {hint} Use --provider or provider/model.",
                    names.join(", ")
                )),
                ..CliModel::default()
            };
        }
    }
    if cli_provider.is_some()
        && let Some(provider) = &provider
    {
        let prefix = format!("{provider}/");
        if cli_model.to_lowercase().starts_with(&prefix.to_lowercase()) {
            pattern = cli_model[prefix.len()..].to_owned();
        }
    }
    let candidates: Vec<Model> = match &provider {
        Some(provider) => models
            .iter()
            .filter(|model| &model.provider == provider)
            .cloned()
            .collect(),
        None => models.clone(),
    };
    let parsed = parse_model_pattern(&pattern, &candidates, false);
    if let Some(model) = parsed.model {
        if inferred_provider {
            let raw: Vec<&Model> = models
                .iter()
                .filter(|other| other.id.to_lowercase() == lower && !models_equal(other, &model))
                .collect();
            if !raw.is_empty() && !registry.provider_has_auth(&model.provider) {
                let authenticated: Vec<&&Model> = raw
                    .iter()
                    .filter(|other| registry.provider_has_auth(&other.provider))
                    .collect();
                if let [one] = authenticated.as_slice() {
                    return found((**one).clone());
                }
            }
        }
        return CliModel {
            model: Some(model),
            thinking_level: parsed.thinking_level,
            warning: parsed.warning,
            error: None,
        };
    }
    if inferred_provider {
        if let Some(exact) = models.iter().find(|model| is_exact(model)) {
            return found(exact.clone());
        }
        let fallback = parse_model_pattern(cli_model, &models, false);
        if fallback.model.is_some() {
            return CliModel {
                model: fallback.model,
                thinking_level: fallback.thinking_level,
                warning: fallback.warning,
                error: None,
            };
        }
    }
    if let Some(provider) = &provider {
        let mut fallback_pattern = pattern.clone();
        let mut fallback_thinking = None;
        if cli_thinking.is_none()
            && let Some(colon) = pattern.rfind(':')
            && let Some(level) = parse_thinking_level(&pattern[colon + 1..])
        {
            fallback_pattern = pattern[..colon].to_owned();
            fallback_thinking = Some(level);
        }
        if let Some(mut model) = build_fallback_model(provider, &fallback_pattern, &models) {
            let requested = cli_thinking.or(fallback_thinking);
            if requested.is_some_and(|level| level != ModelThinkingLevel::Off) {
                model.reasoning = true;
            }
            let note = format!(
                "Model \"{fallback_pattern}\" not found for provider \"{provider}\". Using custom model id."
            );
            let warning = match parsed.warning {
                Some(warning) => format!("{warning} {note}"),
                None => note,
            };
            return CliModel {
                model: Some(model),
                thinking_level: fallback_thinking,
                warning: Some(warning),
                error: None,
            };
        }
    }
    let display = match &provider {
        Some(provider) => format!("{provider}/{pattern}"),
        None => cli_model.to_owned(),
    };
    CliModel {
        model: None,
        thinking_level: None,
        warning: parsed.warning,
        error: Some(format!(
            "Model \"{display}\" not found. Use --list-models to see available models."
        )),
    }
}

/// Pi's `InitialModelResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct InitialModel {
    /// The model, when one is usable.
    pub model: Option<Model>,
    /// Its thinking level.
    pub thinking_level: ModelThinkingLevel,
}

/// Pi's `findInitialModel` without CLI arguments or scoped models: the
/// saved default when its provider has auth, else a known provider's
/// default among available models, else the first available model.
pub fn find_initial_model(
    default_provider: Option<&str>,
    default_model_id: Option<&str>,
    default_thinking_level: Option<ModelThinkingLevel>,
    model_thinking_levels: &[(String, ModelThinkingLevel)],
    registry: &ModelRegistry,
) -> InitialModel {
    if let (Some(provider), Some(id)) = (default_provider, default_model_id)
        && let Some(model) = registry.model(provider, id)
        && registry.has_configured_auth(&model.provider)
    {
        let key = format!("{provider}/{id}");
        let per_model = model_thinking_levels
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, level)| *level);
        return InitialModel {
            model: Some(model),
            thinking_level: per_model
                .or(default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
        };
    }
    let available = registry.available();
    for (provider, id) in DEFAULT_MODEL_PER_PROVIDER {
        if let Some(model) = available
            .iter()
            .find(|model| model.provider == *provider && model.id == *id)
        {
            return InitialModel {
                model: Some(model.clone()),
                thinking_level: DEFAULT_THINKING_LEVEL,
            };
        }
    }
    InitialModel {
        model: available.into_iter().next(),
        thinking_level: DEFAULT_THINKING_LEVEL,
    }
}
