//! Which model and thinking level a child runs on, and the discovery the
//! `list_subagent_models` tool answers.
//!
//! Ported from Bake's TypeScript `packages/subagent/tool-subagent/src/model-selection.ts`
//! (`withoutBlankRoute`, `requestedAgentOptions`, `assertAllowedModelSelection`,
//! `preflightChildLlmRoute`) and `list-models.ts`: blank fields count as
//! omitted, `provider` and `model` go together, a route outside the allowed
//! list is refused naming the allowed routes, and a changed route without
//! `reasoning_effort` takes the new model's default level. Differences: the
//! allowed routes are the models with credentials
//! ([`crate::model_registry::ModelRegistry::available`]), not a settings
//! list; an effort is one of Pi's thinking levels the model supports, so
//! an unsupported one is refused here rather than by the provider; and the
//! listings have no provider display names or effort descriptions, which
//! Pi's registry does not keep.

use bake_ai::{Model, ModelThinkingLevel};

use crate::model_registry::resolver::{DEFAULT_THINKING_LEVEL, parse_thinking_level};
use crate::settings::Settings;

/// The model-facing route fields of a `subagent` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteRequest {
    /// `provider`.
    pub provider: Option<String>,
    /// `model`.
    pub model: Option<String>,
    /// `reasoning_effort`.
    pub reasoning_effort: Option<String>,
}

impl RouteRequest {
    /// The fields of `args`, `withoutBlankRoute`: a blank field names no
    /// route or effort, so it reads as omitted.
    pub fn from_args(args: &serde_json::Value) -> Self {
        let named = |key: &str| {
            args.get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        Self {
            provider: named("provider"),
            model: named("model"),
            reasoning_effort: named("reasoning_effort"),
        }
    }

    /// Whether any field is present.
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none() && self.reasoning_effort.is_none()
    }
}

/// The route a child starts on.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildRoute {
    /// Its model.
    pub model: Model,
    /// Its thinking level; `None` takes the model's default as a new
    /// session would ([`default_level`]).
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// The model's default level for a new session: its per-model setting,
/// the default level, or Pi's, clamped to what the model supports.
pub fn default_level(settings: &Settings, model: &Model) -> ModelThinkingLevel {
    let level = settings
        .model_thinking_level(&model.provider, &model.id)
        .or(settings.default_thinking_level)
        .unwrap_or(DEFAULT_THINKING_LEVEL);
    bake_ai::models::clamp_thinking_level(model, level)
}

/// `allowedRoutesText`: the allowed routes as `provider/model`.
fn allowed_routes(available: &[Model]) -> String {
    let routes: Vec<String> = available
        .iter()
        .map(|model| format!("{}/{}", model.provider, model.id))
        .collect();
    if routes.is_empty() {
        "(none)".to_owned()
    } else {
        routes.join(", ")
    }
}

/// The 0.3 refusal; when the model id is available under another
/// provider, Bake adds those routes first, since models often guess the
/// vendor for a gateway's model.
fn not_allowed(provider: &str, model: &str, available: &[Model]) -> String {
    let elsewhere: Vec<String> = available
        .iter()
        .filter(|candidate| candidate.id == model)
        .map(|candidate| format!("{}/{}", candidate.provider, candidate.id))
        .collect();
    let hint = if elsewhere.is_empty() {
        String::new()
    } else {
        format!(
            " model \"{model}\" is available as {};",
            elsewhere.join(", ")
        )
    };
    format!(
        "child LLM route \"{provider}/{model}\" is not allowed for this Session;{hint} allowed routes: {}",
        allowed_routes(available)
    )
}

fn level_names(levels: &[ModelThinkingLevel]) -> String {
    levels
        .iter()
        .map(|level| level.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The level `effort` names, if `model` supports it.
fn effort(model: &Model, effort: &str) -> Result<ModelThinkingLevel, String> {
    let supported = bake_ai::models::get_supported_thinking_levels(model);
    parse_thinking_level(effort)
        .filter(|level| supported.contains(level))
        .ok_or_else(|| {
            format!(
                "reasoning effort \"{effort}\" is not supported by {}/{}; supported efforts: {}",
                model.provider,
                model.id,
                level_names(&supported)
            )
        })
}

/// Resolves a call's route before anything is created, so a refusal
/// starts nothing. With no field, the child takes the parent's model and
/// level; with only `reasoning_effort`, the parent's model at that level.
/// A route given unchanged keeps the parent's level unless an effort is
/// given; a changed route without one takes the new model's default.
pub fn resolve(
    request: &RouteRequest,
    parent: Option<(&Model, ModelThinkingLevel)>,
    available: &[Model],
) -> Result<ChildRoute, String> {
    if request.provider.is_some() != request.model.is_some() {
        return Err("child LLM `provider` and `model` must be supplied together".to_owned());
    }
    let no_route =
        || "cannot select child LLM values without an effective provider and model".to_owned();
    let (Some(provider), Some(id)) = (&request.provider, &request.model) else {
        let (model, level) = parent.ok_or_else(no_route)?;
        let thinking_level = match &request.reasoning_effort {
            Some(requested) => effort(model, requested)?,
            None => level,
        };
        return Ok(ChildRoute {
            model: model.clone(),
            thinking_level: Some(thinking_level),
        });
    };
    let model = available
        .iter()
        .find(|model| &model.provider == provider && &model.id == id)
        .ok_or_else(|| not_allowed(provider, id, available))?;
    let unchanged = parent
        .is_some_and(|(current, _)| current.provider == model.provider && current.id == model.id);
    let thinking_level = match &request.reasoning_effort {
        Some(requested) => Some(effort(model, requested)?),
        None if unchanged => parent.map(|(_, level)| level),
        None => None,
    };
    Ok(ChildRoute {
        model: model.clone(),
        thinking_level,
    })
}

/// `list_subagent_models`' answer: the providers, a provider's models, or
/// one model and its efforts. Blank fields count as omitted.
pub fn list(
    args: &serde_json::Value,
    available: &[Model],
    settings: &Settings,
) -> Result<String, String> {
    let request = RouteRequest::from_args(args);
    if request.model.is_some() && request.provider.is_none() {
        return Err("`model` requires `provider`".to_owned());
    }
    let Some(provider) = &request.provider else {
        let mut providers: Vec<(&str, usize)> = Vec::new();
        for model in available {
            match providers.iter_mut().find(|(id, _)| *id == model.provider) {
                Some((_, count)) => *count += 1,
                None => providers.push((&model.provider, 1)),
            }
        }
        if providers.is_empty() {
            return Ok("(no LLM providers)".to_owned());
        }
        return Ok(providers
            .iter()
            .map(|(id, count)| {
                format!(
                    "{id} ({count} {})",
                    if *count == 1 { "model" } else { "models" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"));
    };
    let models: Vec<&Model> = available
        .iter()
        .filter(|model| &model.provider == provider)
        .collect();
    if models.is_empty() {
        return Err(format!(
            "LLM provider \"{provider}\" is not allowed for this Session; allowed routes: {}",
            allowed_routes(available)
        ));
    }
    let line = |model: &Model| format!("{}/{} — {}", model.provider, model.id, model.name);
    let Some(id) = &request.model else {
        return Ok(models
            .iter()
            .map(|model| line(model))
            .collect::<Vec<_>>()
            .join("\n"));
    };
    let model = models
        .iter()
        .find(|model| &model.id == id)
        .ok_or_else(|| not_allowed(provider, id, available))?;
    let default = default_level(settings, model);
    let efforts = bake_ai::models::get_supported_thinking_levels(model)
        .iter()
        .map(|level| {
            if *level == default {
                format!("{} (default)", level.as_str())
            } else {
                level.as_str().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("{}\nReasoning efforts:\n{efforts}", line(model)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(provider: &str, id: &str, reasoning: bool) -> Model {
        let mut model = bake_ai::providers::faux::faux_model(id);
        model.provider = provider.to_owned();
        model.name = format!("{id} name");
        model.reasoning = reasoning;
        model
    }

    fn request(args: serde_json::Value) -> RouteRequest {
        RouteRequest::from_args(&args)
    }

    #[test]
    fn omitted_fields_inherit_the_parent_route() {
        let parent = model("p", "a", true);
        let available = vec![parent.clone(), model("p", "b", true)];
        let route = resolve(
            &request(json!({ "provider": " ", "model": "" })),
            Some((&parent, ModelThinkingLevel::High)),
            &available,
        )
        .expect("route");
        assert_eq!(route.model.id, "a");
        assert_eq!(route.thinking_level, Some(ModelThinkingLevel::High));
        let effort_only = resolve(
            &request(json!({ "reasoning_effort": "low" })),
            Some((&parent, ModelThinkingLevel::High)),
            &available,
        )
        .expect("route");
        assert_eq!(effort_only.thinking_level, Some(ModelThinkingLevel::Low));
    }

    #[test]
    fn a_changed_route_takes_its_default_unless_an_effort_is_given() {
        let parent = model("p", "a", true);
        let available = vec![parent.clone(), model("p", "b", true)];
        let parent = Some((&parent, ModelThinkingLevel::High));
        let changed = resolve(
            &request(json!({ "provider": "p", "model": "b" })),
            parent,
            &available,
        )
        .expect("route");
        assert_eq!(
            (changed.model.id.as_str(), changed.thinking_level),
            ("b", None)
        );
        let same = resolve(
            &request(json!({ "provider": "p", "model": "a" })),
            parent,
            &available,
        )
        .expect("route");
        assert_eq!(same.thinking_level, Some(ModelThinkingLevel::High));
        let chosen = resolve(
            &request(json!({ "provider": "p", "model": "b", "reasoning_effort": "minimal" })),
            parent,
            &available,
        )
        .expect("route");
        assert_eq!(chosen.thinking_level, Some(ModelThinkingLevel::Minimal));
    }

    #[test]
    fn refusals_name_what_is_allowed() {
        let parent = model("p", "a", true);
        let plain = model("q", "plain", false);
        let available = vec![parent.clone(), plain];
        let parent = Some((&parent, ModelThinkingLevel::High));
        assert_eq!(
            resolve(&request(json!({ "provider": "p" })), parent, &available).unwrap_err(),
            "child LLM `provider` and `model` must be supplied together"
        );
        assert_eq!(
            resolve(
                &request(json!({ "provider": "p", "model": "z" })),
                parent,
                &available
            )
            .unwrap_err(),
            "child LLM route \"p/z\" is not allowed for this Session; allowed routes: p/a, q/plain"
        );
        assert_eq!(
            resolve(
                &request(json!({ "provider": "x", "model": "plain" })),
                parent,
                &available
            )
            .unwrap_err(),
            "child LLM route \"x/plain\" is not allowed for this Session; model \"plain\" is available as q/plain; allowed routes: p/a, q/plain"
        );
        assert_eq!(
            resolve(
                &request(json!({ "provider": "q", "model": "plain", "reasoning_effort": "high" })),
                parent,
                &available
            )
            .unwrap_err(),
            "reasoning effort \"high\" is not supported by q/plain; supported efforts: off"
        );
        assert_eq!(
            resolve(
                &request(json!({ "reasoning_effort": "turbo" })),
                parent,
                &available
            )
            .unwrap_err()
            .split(';')
            .next(),
            Some("reasoning effort \"turbo\" is not supported by p/a")
        );
        assert!(
            resolve(&RouteRequest::default(), None, &available)
                .unwrap_err()
                .starts_with("cannot select child LLM values")
        );
    }

    #[test]
    fn listings_follow_the_request() {
        let available = vec![
            model("p", "a", true),
            model("p", "b", false),
            model("q", "c", false),
        ];
        let settings = Settings::default();
        assert_eq!(
            list(&json!({}), &available, &settings).expect("providers"),
            "p (2 models)\nq (1 model)"
        );
        assert_eq!(
            list(&json!({ "provider": "p" }), &available, &settings).expect("models"),
            "p/a — a name\np/b — b name"
        );
        let efforts = list(
            &json!({ "provider": "p", "model": "a" }),
            &available,
            &settings,
        )
        .expect("efforts");
        assert!(efforts.starts_with("p/a — a name\nReasoning efforts:\noff\n"));
        assert!(efforts.contains("medium (default)"));
        assert_eq!(
            list(&json!({ "model": "a" }), &available, &settings).unwrap_err(),
            "`model` requires `provider`"
        );
        assert!(
            list(&json!({ "provider": "z" }), &available, &settings)
                .unwrap_err()
                .starts_with("LLM provider \"z\" is not allowed for this Session")
        );
        assert_eq!(
            list(&json!({}), &[], &settings).expect("none"),
            "(no LLM providers)"
        );
    }
}
