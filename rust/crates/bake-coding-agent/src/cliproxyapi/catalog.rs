//! The proxy's catalog and each model's wire protocol: `cliProxyApi`,
//! `cliProxyModels`, and `CliProxyModel` in
//! `apps/tui/packages/app/src/cliproxyapi.ts`.

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use super::endpoints::js_trim;

/// The wire protocols a login assigns; the route's own is
/// `openai-responses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliProxyApi {
    /// OpenAI Responses, the route's protocol.
    OpenAiResponses,
    /// OpenAI Chat Completions.
    OpenAiCompletions,
    /// Anthropic Messages.
    AnthropicMessages,
}

impl CliProxyApi {
    /// The protocol's name in Pi's `api` field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiCompletions => "openai-completions",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

/// Families whose upstreams speak Chat Completions only
/// (`CHAT_COMPLETIONS_FAMILY`). CPA serves them on `/v1/responses` by
/// translating to chat itself, and a strict upstream rejects shapes that
/// translation produces, so these models skip the translation.
const CHAT_COMPLETIONS_FAMILIES: [&str; 7] = [
    "kimi-",
    "moonshot-",
    "glm-",
    "qwen",
    "qwq-",
    "deepseek-",
    "minimax-",
];

/// Whether `id` starts, after an optional `vendor/` or `vendor:` namespace,
/// with one of `families`, ignoring ASCII case. This is
/// `/^(?:[\w.-]+[/:])?(?:…)/i` without a regex: JavaScript's `\w` and `/i`
/// without the `u` flag are ASCII-only here, and the namespace class cannot
/// match `/` or `:`, so the only namespace the pattern can take is the
/// longest run of `[\w.-]` followed by one of those.
fn matches_family(id: &str, families: &[&str]) -> bool {
    let at = |start: usize| {
        id.get(start..).is_some_and(|rest| {
            families.iter().any(|family| {
                rest.get(..family.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(family))
            })
        })
    };
    if at(0) {
        return true;
    }
    let run = id
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        .count();
    run > 0 && matches!(id.as_bytes().get(run), Some(b'/' | b':')) && at(run + 1)
}

/// `cliProxyApi`: the wire protocol one listed model is served over.
///
/// Claude, by id wherever it is hosted or by an `anthropic` owner, goes over
/// Anthropic Messages, whose `cache_control` breakpoints the proxy passes
/// on; the Chat-Completions-only families go over Chat Completions; OpenAI,
/// Gemini, and Grok stay on Responses.
pub fn cli_proxy_api(id: &str, owned_by: Option<&str>) -> CliProxyApi {
    if owned_by.is_some_and(|owner| owner.to_lowercase() == "anthropic")
        || matches_family(id, &["claude-"])
    {
        return CliProxyApi::AnthropicMessages;
    }
    if matches_family(id, &CHAT_COMPLETIONS_FAMILIES) {
        return CliProxyApi::OpenAiCompletions;
    }
    CliProxyApi::OpenAiResponses
}

/// A request modality the route declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    /// `text`
    Text,
    /// `image`
    Image,
}

impl Modality {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            _ => None,
        }
    }
}

/// The reasoning levels a listing may offer (`supported_reasoning_levels`).
const LISTED_EFFORTS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

/// `CliProxyModel`: the fields a login saves for one discovered model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProxyModel {
    /// The proxy's model id.
    pub id: String,
    /// Set only where the model leaves the route's `openai-responses`.
    pub api: Option<CliProxyApi>,
    /// Set with `anthropic-messages`: the proxy root.
    pub base_url: Option<String>,
    /// Display name.
    pub name: String,
    /// Context window in tokens.
    pub context_window: Option<u64>,
    /// Output limit in tokens.
    pub max_tokens: Option<u64>,
    /// Request modalities, text first.
    pub input: Option<Vec<Modality>>,
    /// Offered levels mapped to themselves, in listing order.
    pub reasoning_efforts: Option<Vec<String>>,
    /// `compat.forceAdaptiveThinking`, set on Claude models that take an
    /// effort level.
    pub force_adaptive_thinking: bool,
}

impl CliProxyModel {
    /// The model as the 0.3 login saved it in `settings.yaml`, with the
    /// TypeScript's key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(self.id));
        if let Some(api) = self.api {
            out.insert("api".into(), json!(api.as_str()));
        }
        if let Some(base_url) = &self.base_url {
            out.insert("baseURL".into(), json!(base_url));
        }
        out.insert("name".into(), json!(self.name));
        if let Some(context_window) = self.context_window {
            out.insert("contextWindow".into(), json!(context_window));
        }
        if let Some(max_tokens) = self.max_tokens {
            out.insert("maxTokens".into(), json!(max_tokens));
        }
        if let Some(input) = &self.input {
            let input: Vec<&str> = input.iter().map(|modality| modality.as_str()).collect();
            out.insert("input".into(), json!(input));
        }
        if let Some(efforts) = &self.reasoning_efforts {
            let map: Map<String, Value> = efforts
                .iter()
                .map(|level| (level.clone(), json!(level)))
                .collect();
            out.insert("reasoningEfforts".into(), Value::Object(map));
        }
        if self.force_adaptive_thinking {
            out.insert("compat".into(), json!({ "forceAdaptiveThinking": true }));
        }
        Value::Object(out)
    }
}

/// Why a catalog is not a model list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidModelList;

impl std::fmt::Display for InvalidModelList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CLIProxyAPI returned an invalid model list")
    }
}

impl std::error::Error for InvalidModelList {}

/// `positiveInteger`: the first value that is a positive integral number.
/// JavaScript accepts any integral double; this takes those up to
/// `u64::MAX` and skips larger ones.
fn positive_integer(values: &[Option<&Value>]) -> Option<u64> {
    values.iter().flatten().find_map(|value| {
        let number = value.as_number()?;
        if let Some(integer) = number.as_u64() {
            return (integer > 0).then_some(integer);
        }
        let float = number.as_f64()?;
        // `u64::MAX as f64` rounds up to 2^64, which is out of range.
        (float.is_finite() && float.fract() == 0.0 && float > 0.0 && float < u64::MAX as f64)
            .then_some(float as u64)
    })
}

fn string_field<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

fn non_blank<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    string_field(entry, key)
        .map(js_trim)
        .filter(|value| !value.is_empty())
}

/// `cliProxyModels`: turn CPA's `/v1/models` reply into the explicit models
/// a custom route needs.
///
/// The list is the reply itself when it is an array, else its `models`
/// array, else its `data`. Entries that are not objects, have no id, are
/// hidden, or cannot answer in text are skipped; a later entry with an id
/// already seen replaces the earlier one in its place. `root`, the proxy
/// root, becomes the base URL of each Anthropic Messages model.
pub fn cli_proxy_models(
    payload: &Value,
    root: Option<&str>,
) -> Result<Vec<CliProxyModel>, InvalidModelList> {
    let entries = match payload {
        Value::Array(entries) => entries,
        Value::Object(record) => match (record.get("models"), record.get("data")) {
            (Some(Value::Array(models)), _) => models,
            (_, Some(Value::Array(data))) => data,
            _ => return Err(InvalidModelList),
        },
        _ => return Err(InvalidModelList),
    };
    let mut models: IndexMap<String, CliProxyModel> = IndexMap::new();
    for value in entries {
        let Value::Object(entry) = value else {
            continue;
        };
        let id = non_blank(entry, "slug")
            .or_else(|| string_field(entry, "id").map(js_trim))
            .unwrap_or("");
        let hidden = string_field(entry, "visibility")
            .is_some_and(|visibility| visibility.to_lowercase() == "hide");
        if id.is_empty() || hidden {
            continue;
        }
        // An image or video generator cannot answer a chat turn.
        if let Some(Value::Array(output)) = entry.get("output_modalities")
            && !output.iter().any(|item| item.as_str() == Some("text"))
        {
            continue;
        }
        let name = non_blank(entry, "display_name")
            .or_else(|| non_blank(entry, "name"))
            .unwrap_or(id);
        let context_window =
            positive_integer(&[entry.get("context_window"), entry.get("max_context_window")]);
        let max_tokens = positive_integer(&[
            entry.get("max_tokens"),
            entry.get("max_output_tokens"),
            entry.get("max_completion_tokens"),
        ]);
        let input: Vec<Modality> = match entry.get("input_modalities") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().and_then(Modality::parse))
                .collect(),
            _ => Vec::new(),
        };
        let efforts: Vec<&str> = match entry.get("supported_reasoning_levels") {
            Some(Value::Array(levels)) => levels
                .iter()
                .filter_map(|level| match level {
                    Value::String(level) => Some(level.as_str()),
                    Value::Object(level) => level.get("effort").and_then(Value::as_str),
                    _ => None,
                })
                .filter(|level| LISTED_EFFORTS.contains(level))
                .collect(),
            _ => Vec::new(),
        };
        let mut distinct: Vec<String> = Vec::new();
        for level in &efforts {
            if !distinct.iter().any(|seen| seen == level) {
                distinct.push((*level).to_owned());
            }
        }
        let api = cli_proxy_api(id, string_field(entry, "owned_by"));
        // Adaptive thinking is what takes an effort level. The Claude models
        // that list `xhigh` or `max` accept it; older ones answer it with
        // HTTP 400 and keep budget thinking.
        let adaptive = api == CliProxyApi::AnthropicMessages
            && efforts
                .iter()
                .any(|level| matches!(*level, "xhigh" | "max"));
        let input = if input.is_empty() {
            None
        } else if input.contains(&Modality::Text) {
            Some(input)
        } else {
            Some(std::iter::once(Modality::Text).chain(input).collect())
        };
        models.insert(
            id.to_owned(),
            CliProxyModel {
                id: id.to_owned(),
                api: (api != CliProxyApi::OpenAiResponses).then_some(api),
                base_url: root
                    .filter(|_| api == CliProxyApi::AnthropicMessages)
                    .map(str::to_owned),
                name: name.to_owned(),
                context_window,
                max_tokens,
                input,
                reasoning_efforts: (!efforts.is_empty()).then_some(distinct),
                force_adaptive_thinking: adaptive,
            },
        );
    }
    Ok(models.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models_json(payload: Value, root: Option<&str>) -> Value {
        let models = cli_proxy_models(&payload, root).unwrap_or_default();
        Value::Array(models.iter().map(CliProxyModel::to_json).collect())
    }

    // cliproxyapi.test.ts: "CLIProxyAPI models" > "uses proxy slugs and
    // capability metadata while skipping hidden entries".
    #[test]
    fn uses_proxy_slugs_and_capability_metadata_while_skipping_hidden_entries() {
        assert_eq!(
            models_json(
                json!({ "models": [
                    { "slug": "gpt-test", "display_name": "GPT Test", "context_window": 128000,
                      "max_output_tokens": 8192, "input_modalities": ["image"],
                      "supported_reasoning_levels": [{ "effort": "low" }, { "effort": "high" }] },
                    { "slug": "hidden", "visibility": "hide" },
                ] }),
                None
            ),
            json!([{
                "id": "gpt-test", "name": "GPT Test", "contextWindow": 128000, "maxTokens": 8192,
                "input": ["text", "image"], "reasoningEfforts": { "low": "low", "high": "high" },
            }])
        );
        assert_eq!(
            models_json(json!({ "data": [{ "id": "model-a" }] }), None),
            json!([{ "id": "model-a", "name": "model-a" }])
        );
    }

    // cliproxyapi.test.ts: "CLIProxyAPI models" > "serves each family over
    // the protocol it relays cleanly".
    #[test]
    fn serves_each_family_over_the_protocol_it_relays_cleanly() {
        let ids = [
            "kimi-k3",
            "moonshotai/kimi-k3-256k",
            "glm-5.3-flash",
            "qwen3-coder-plus",
            "deepseek-v4-pro",
            "MiniMax-M3",
            "gpt-6-sol",
            "gpt-6.1-sol",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "anthropic/claude-sonnet-5",
            "gemini-3.8-flash-high",
            "grok-4.7",
        ];
        let payload =
            json!({ "data": ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>() });
        let apis: Vec<(String, Option<&str>)> = cli_proxy_models(&payload, None)
            .unwrap_or_default()
            .into_iter()
            .map(|model| (model.id, model.api.map(CliProxyApi::as_str)))
            .collect();
        let completions = Some("openai-completions");
        let messages = Some("anthropic-messages");
        let expected: Vec<(String, Option<&str>)> = [
            ("kimi-k3", completions),
            ("moonshotai/kimi-k3-256k", completions),
            ("glm-5.3-flash", completions),
            ("qwen3-coder-plus", completions),
            ("deepseek-v4-pro", completions),
            ("MiniMax-M3", completions),
            ("gpt-6-sol", None),
            ("gpt-6.1-sol", None),
            ("claude-opus-5-5", messages),
            ("claude-sonnet-5-5", messages),
            ("anthropic/claude-sonnet-5", messages),
            ("gemini-3.8-flash-high", None),
            ("grok-4.7", None),
        ]
        .into_iter()
        .map(|(id, api)| (id.to_owned(), api))
        .collect();
        assert_eq!(apis, expected);
        // The listing's owner decides for an id that does not name its family.
        assert_eq!(
            cli_proxy_api("house-model", Some("Anthropic")),
            CliProxyApi::AnthropicMessages
        );
        assert_eq!(
            cli_proxy_api("house-model", Some("openai")),
            CliProxyApi::OpenAiResponses
        );
    }

    // The namespace and family patterns' edges, each checked against the
    // TypeScript regular expressions in Node.
    #[test]
    fn matches_the_family_patterns_exactly() {
        let api = |id: &str| cli_proxy_api(id, None).as_str();
        assert_eq!(api("vendor:GLM-5"), "openai-completions");
        assert_eq!(api("a.b_c-d/qwq-32b"), "openai-completions");
        assert_eq!(api("qwen"), "openai-completions");
        assert_eq!(api("kimi"), "openai-responses");
        assert_eq!(api("a/b/kimi-k3"), "openai-responses");
        assert_eq!(api("/kimi-k3"), "openai-responses");
        assert_eq!(api("x/CLAUDE-opus"), "anthropic-messages");
        assert_eq!(api("x claude-opus"), "openai-responses");
        assert_eq!(api("é/claude-opus"), "openai-responses");
        assert_eq!(api("\u{212A}imi-k3"), "openai-responses");
        assert_eq!(api(""), "openai-responses");
    }

    // cliproxyapi.test.ts: "CLIProxyAPI models" > "sends Claude to the proxy
    // root, with adaptive thinking only where it takes an effort level".
    #[test]
    fn sends_claude_to_the_proxy_root_with_adaptive_thinking_only_where_it_takes_an_effort_level() {
        assert_eq!(
            models_json(
                json!({ "data": [
                    { "id": "claude-opus-5-5", "owned_by": "anthropic",
                      "supported_reasoning_levels": ["none", "low", "high", "xhigh", "max"] },
                    { "id": "claude-sonnet-5-5", "owned_by": "anthropic",
                      "supported_reasoning_levels": ["none", "low", "medium", "high", "xhigh", "max"] },
                    { "id": "claude-opus-4-5-20251101", "owned_by": "anthropic",
                      "supported_reasoning_levels": ["none", "low", "high"] },
                    { "id": "gpt-6-sol", "owned_by": "openai", "supported_reasoning_levels": ["low", "high"] },
                ] }),
                Some("https://proxy.example")
            ),
            json!([
                { "id": "claude-opus-5-5", "api": "anthropic-messages", "baseURL": "https://proxy.example",
                  "name": "claude-opus-5-5",
                  "reasoningEfforts": { "low": "low", "high": "high", "xhigh": "xhigh", "max": "max" },
                  "compat": { "forceAdaptiveThinking": true } },
                { "id": "claude-sonnet-5-5", "api": "anthropic-messages", "baseURL": "https://proxy.example",
                  "name": "claude-sonnet-5-5",
                  "reasoningEfforts": { "low": "low", "medium": "medium", "high": "high", "xhigh": "xhigh",
                                        "max": "max" },
                  "compat": { "forceAdaptiveThinking": true } },
                { "id": "claude-opus-4-5-20251101", "api": "anthropic-messages",
                  "baseURL": "https://proxy.example", "name": "claude-opus-4-5-20251101",
                  "reasoningEfforts": { "low": "low", "high": "high" } },
                { "id": "gpt-6-sol", "name": "gpt-6-sol", "reasoningEfforts": { "low": "low", "high": "high" } },
            ])
        );
    }

    // cliproxyapi.test.ts: "CLIProxyAPI models" > "leaves out models that
    // cannot answer in text".
    #[test]
    fn leaves_out_models_that_cannot_answer_in_text() {
        let ids: Vec<String> = cli_proxy_models(
            &json!({ "data": [
                { "id": "gpt-image-2", "output_modalities": ["image"] },
                { "id": "gpt-6-sol", "output_modalities": ["text"] },
                { "id": "unlabelled" },
            ] }),
            None,
        )
        .unwrap_or_default()
        .into_iter()
        .map(|model| model.id)
        .collect();
        assert_eq!(ids, ["gpt-6-sol", "unlabelled"]);
    }

    // Adaptive thinking follows the protocol and either top level alone, as
    // the TypeScript gives it.
    #[test]
    fn takes_adaptive_thinking_only_over_anthropic_messages() {
        assert_eq!(
            models_json(
                json!({ "data": [
                    { "id": "claude-y", "supported_reasoning_levels": ["max"] },
                    { "id": "gpt-x", "supported_reasoning_levels": ["xhigh"] },
                    { "id": "house", "owned_by": "ANTHROPIC", "supported_reasoning_levels": ["xhigh"] },
                ] }),
                Some("https://r")
            ),
            json!([
                { "id": "claude-y", "api": "anthropic-messages", "baseURL": "https://r", "name": "claude-y",
                  "reasoningEfforts": { "max": "max" }, "compat": { "forceAdaptiveThinking": true } },
                { "id": "gpt-x", "name": "gpt-x", "reasoningEfforts": { "xhigh": "xhigh" } },
                { "id": "house", "api": "anthropic-messages", "baseURL": "https://r", "name": "house",
                  "reasoningEfforts": { "xhigh": "xhigh" }, "compat": { "forceAdaptiveThinking": true } },
            ])
        );
    }

    // cliProxyModels' remaining branches, each checked against the
    // TypeScript in Node: the list's three homes and its error, slug and id
    // fallbacks and trimming, limit fallbacks and non-integers, duplicate
    // modalities and efforts, objects without an effort, and a later
    // duplicate id replacing the earlier one in its place.
    #[test]
    fn follows_the_remaining_typescript_branches() {
        assert_eq!(
            cli_proxy_models(&json!({ "data": {} }), None),
            Err(InvalidModelList)
        );
        assert_eq!(cli_proxy_models(&json!("x"), None), Err(InvalidModelList));
        assert_eq!(cli_proxy_models(&json!(null), None), Err(InvalidModelList));
        assert_eq!(
            InvalidModelList.to_string(),
            "CLIProxyAPI returned an invalid model list"
        );
        // `models` wins over `data` only when it is an array.
        assert_eq!(
            models_json(json!({ "models": 1, "data": [{ "id": "d" }] }), None),
            json!([{ "id": "d", "name": "d" }])
        );
        assert_eq!(
            models_json(
                json!([
                    "bare", null, [1], { "slug": "  ", "id": " spaced " },
                    { "slug": 3, "id": "n", "display_name": " ", "name": " Named " },
                    { "id": "h", "visibility": "HIDE" },
                    { "id": "limits", "context_window": 0, "max_context_window": 4096.0,
                      "max_tokens": 1.5, "max_output_tokens": -2, "max_completion_tokens": 77 },
                    { "id": "dup-input", "input_modalities": ["image", "image", "audio", 1] },
                    { "id": "efforts", "supported_reasoning_levels": ["high", { "effort": "high" }, ["low"],
                      { "level": "low" }, "none", "minimal"] },
                    { "id": "claude-x", "supported_reasoning_levels": ["high"] },
                    { "id": "spaced", "name": "Second" },
                    { "id": "" },
                    { "id": "no-text", "output_modalities": [] },
                ]),
                Some("https://r")
            ),
            json!([
                { "id": "spaced", "name": "Second" },
                { "id": "n", "name": "Named" },
                { "id": "limits", "name": "limits", "contextWindow": 4096, "maxTokens": 77 },
                { "id": "dup-input", "name": "dup-input", "input": ["text", "image", "image"] },
                { "id": "efforts", "name": "efforts",
                  "reasoningEfforts": { "high": "high", "minimal": "minimal" } },
                { "id": "claude-x", "api": "anthropic-messages", "baseURL": "https://r", "name": "claude-x",
                  "reasoningEfforts": { "high": "high" } },
            ])
        );
    }
}
