//! `models.json`: custom providers and models, without credentials.
//!
//! Ported from Pi `packages/coding-agent/src/core/model-config.ts` and
//! `stripJsonComments` of `src/utils/json.ts` (v1.1.0). Pi reads
//! `~/.pi/agent/models.json`; Bake reads `$BAKE_HOME/models.json`. Line
//! comments (`//`) and trailing commas are allowed.
//!
//! # Deviations from Pi
//!
//! Pi validates the file against a TypeBox schema. This module checks the
//! members the session reads with the same paths and English messages
//! TypeBox reports (`providers.p.baseUrl: must be string`), for types,
//! minimum lengths, required members, and the `text`/`image` and `radius`
//! literals. Nested objects it passes through (`compat`, `thinkingLevelMap`,
//! `inputLimits`, `promptCache`, sampling parameters, and cost tiers) are
//! checked only to be objects or arrays; a wrong member inside one is
//! reported when the model is built, as a provider error. A file larger
//! than [`MAX_MODELS_FILE_BYTES`] fails to load.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::auth_storage::{read_text_file, strip_bom};

/// The largest `models.json` read, in bytes.
pub const MAX_MODELS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A JSON object in insertion order.
pub type JsonObject = Map<String, Value>;

/// Pi's `stripJsonComments`: removes `//` line comments and trailing commas
/// outside string literals.
pub fn strip_json_comments(input: &str) -> String {
    static COMMENTS: OnceLock<Option<Regex>> = OnceLock::new();
    static COMMAS: OnceLock<Option<Regex>> = OnceLock::new();
    let comments = COMMENTS.get_or_init(|| Regex::new(r#""(?:\\.|[^"\\])*"|//[^\n]*"#).ok());
    let commas = COMMAS.get_or_init(|| Regex::new(r#""(?:\\.|[^"\\])*"|,(\s*[}\]])"#).ok());
    let (Some(comments), Some(commas)) = (comments, commas) else {
        return input.to_owned();
    };
    let without_comments = comments.replace_all(input, |captures: &regex::Captures<'_>| {
        let matched = &captures[0];
        if matched.starts_with('"') {
            matched.to_owned()
        } else {
            String::new()
        }
    });
    commas
        .replace_all(
            &without_comments,
            |captures: &regex::Captures<'_>| match captures.get(1) {
                Some(tail) => tail.as_str().to_owned(),
                None => captures[0].to_owned(),
            },
        )
        .into_owned()
}

#[derive(Default)]
struct Errors(Vec<String>);

impl Errors {
    fn push(&mut self, path: &str, message: &str) {
        let path = if path.is_empty() { "root" } else { path };
        self.0.push(format!("  - {path}: {message}"));
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

#[derive(Clone, Copy)]
enum Kind {
    String,
    NonEmptyString,
    Number,
    Boolean,
    Object,
    Array,
    StringRecord,
    Modalities,
}

fn check(errors: &mut Errors, path: &str, value: &Value, kind: Kind) {
    match kind {
        Kind::String => {
            if !value.is_string() {
                errors.push(path, "must be string");
            }
        }
        Kind::NonEmptyString => match value.as_str() {
            Some("") => errors.push(path, "must not have fewer than 1 characters"),
            Some(_) => {}
            None => errors.push(path, "must be string"),
        },
        Kind::Number => {
            if !value.is_number() {
                errors.push(path, "must be number");
            }
        }
        Kind::Boolean => {
            if !value.is_boolean() {
                errors.push(path, "must be boolean");
            }
        }
        Kind::Object => {
            if !value.is_object() {
                errors.push(path, "must be object");
            }
        }
        Kind::Array => {
            if !value.is_array() {
                errors.push(path, "must be array");
            }
        }
        Kind::StringRecord => match value.as_object() {
            Some(object) => {
                for (key, entry) in object {
                    check(errors, &join(path, key), entry, Kind::String);
                }
            }
            None => errors.push(path, "must be object"),
        },
        Kind::Modalities => match value.as_array() {
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    if !matches!(item.as_str(), Some("text" | "image")) {
                        let path = join(path, &index.to_string());
                        errors.push(&path, "must be equal to constant");
                        errors.push(&path, "must be equal to constant");
                        errors.push(&path, "must match a schema in anyOf");
                    }
                }
            }
            None => errors.push(path, "must be array"),
        },
    }
}

fn check_members(errors: &mut Errors, path: &str, object: &JsonObject, members: &[(&str, Kind)]) {
    for (name, kind) in members {
        if let Some(value) = object.get(*name) {
            check(errors, &join(path, name), value, *kind);
        }
    }
}

fn check_cost(errors: &mut Errors, path: &str, value: &Value, required: bool) {
    let Some(cost) = value.as_object() else {
        errors.push(path, "must be object");
        return;
    };
    for name in ["input", "output", "cacheRead", "cacheWrite"] {
        match cost.get(name) {
            Some(value) => check(errors, &join(path, name), value, Kind::Number),
            None if required => errors.push(
                &join(path, name),
                &format!("must have required properties {name}"),
            ),
            None => {}
        }
    }
    if let Some(tiers) = cost.get("tiers") {
        check(errors, &join(path, "tiers"), tiers, Kind::Array);
    }
}

const MODEL_COMMON: &[(&str, Kind)] = &[
    ("name", Kind::NonEmptyString),
    ("reasoning", Kind::Boolean),
    ("thinkingLevelMap", Kind::Object),
    ("input", Kind::Modalities),
    ("inputLimits", Kind::Object),
    ("promptCache", Kind::Object),
    ("contextWindow", Kind::Number),
    ("maxTokens", Kind::Number),
    ("samplingParams", Kind::Object),
    ("samplingParamsByThinkingLevel", Kind::Object),
    ("headers", Kind::StringRecord),
    ("compat", Kind::Object),
];

fn check_model(errors: &mut Errors, path: &str, value: &Value) {
    let Some(model) = value.as_object() else {
        errors.push(path, "must be object");
        return;
    };
    match model.get("id") {
        Some(id) => check(errors, &join(path, "id"), id, Kind::NonEmptyString),
        None => errors.push(&join(path, "id"), "must have required properties id"),
    }
    check_members(
        errors,
        path,
        model,
        &[
            ("api", Kind::NonEmptyString),
            ("baseUrl", Kind::NonEmptyString),
        ],
    );
    check_members(errors, path, model, MODEL_COMMON);
    if let Some(cost) = model.get("cost") {
        check_cost(errors, &join(path, "cost"), cost, true);
    }
}

fn check_provider(errors: &mut Errors, path: &str, value: &Value) {
    let Some(provider) = value.as_object() else {
        errors.push(path, "must be object");
        return;
    };
    check_members(
        errors,
        path,
        provider,
        &[
            ("name", Kind::NonEmptyString),
            ("baseUrl", Kind::NonEmptyString),
            ("apiKey", Kind::NonEmptyString),
            ("api", Kind::NonEmptyString),
            ("headers", Kind::StringRecord),
            ("compat", Kind::Object),
            ("authHeader", Kind::Boolean),
        ],
    );
    if let Some(oauth) = provider.get("oauth")
        && oauth.as_str() != Some("radius")
    {
        errors.push(&join(path, "oauth"), "must be equal to constant");
    }
    match provider.get("models") {
        Some(Value::Array(models)) => {
            for (index, model) in models.iter().enumerate() {
                check_model(
                    errors,
                    &join(&join(path, "models"), &index.to_string()),
                    model,
                );
            }
        }
        Some(_) => errors.push(&join(path, "models"), "must be array"),
        None => {}
    }
    match provider.get("modelOverrides") {
        Some(Value::Object(overrides)) => {
            for (id, value) in overrides {
                let path = join(&join(path, "modelOverrides"), id);
                match value.as_object() {
                    Some(model) => {
                        check_members(errors, &path, model, MODEL_COMMON);
                        if let Some(cost) = model.get("cost") {
                            check_cost(errors, &join(&path, "cost"), cost, false);
                        }
                    }
                    None => errors.push(&path, "must be object"),
                }
            }
        }
        Some(_) => errors.push(&join(path, "modelOverrides"), "must be object"),
        None => {}
    }
}

/// Validates a parsed `models.json`, returning Pi's indented error lines.
fn validate(value: &Value) -> Result<(), Vec<String>> {
    let mut errors = Errors::default();
    match value.as_object() {
        None => errors.push("", "must be object"),
        Some(root) => match root.get("providers") {
            None => errors.push("providers", "must have required properties providers"),
            Some(Value::Object(providers)) => {
                for (id, provider) in providers {
                    check_provider(&mut errors, &join("providers", id), provider);
                }
            }
            Some(_) => errors.push("providers", "must be object"),
        },
    }
    if errors.0.is_empty() {
        Ok(())
    } else {
        Err(errors.0)
    }
}

/// One load of `models.json`, Pi's `ModelConfig`: providers in file order,
/// or the load error.
#[derive(Debug, Clone, Default)]
pub struct ModelConfig {
    providers: Vec<(String, JsonObject)>,
    error: Option<String>,
}

impl ModelConfig {
    /// No providers.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Pi's `ModelConfig.load`: no file is no providers; a file that fails
    /// to read, parse, or validate is no providers and an error.
    pub fn load(path: &Path) -> Self {
        let display = PathBuf::from(path);
        let content = match read_text_file(path, MAX_MODELS_FILE_BYTES) {
            Ok(None) => return Self::empty(),
            Ok(Some(content)) => content,
            Err(error) => {
                return Self::failed(format!(
                    "Failed to load models.json: {error}\n\nFile: {}",
                    display.display()
                ));
            }
        };
        Self::parse(&content, &display.display().to_string())
    }

    /// Parses `models.json` text; `file` names it in errors.
    pub fn parse(content: &str, file: &str) -> Self {
        let parsed = match serde_json::from_str::<Value>(&strip_json_comments(strip_bom(content))) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Self::failed(format!(
                    "Failed to parse models.json: {error}\n\nFile: {file}"
                ));
            }
        };
        Self::from_value(parsed, file)
    }

    /// A parsed `models.json`; `file` names it in errors.
    pub fn from_value(value: Value, file: &str) -> Self {
        if let Err(errors) = validate(&value) {
            return Self::failed(format!(
                "Invalid models.json schema:\n{}\n\nFile: {file}",
                errors.join("\n")
            ));
        }
        let providers = match value {
            Value::Object(mut root) => match root.remove("providers") {
                Some(Value::Object(providers)) => providers
                    .into_iter()
                    .filter_map(|(id, provider)| match provider {
                        Value::Object(provider) => Some((id, provider)),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        Self {
            providers,
            error: None,
        }
    }

    fn failed(error: String) -> Self {
        Self {
            providers: Vec::new(),
            error: Some(error),
        }
    }

    /// The provider config for `id`.
    pub fn provider(&self, id: &str) -> Option<&JsonObject> {
        self.providers
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, provider)| provider)
    }

    /// Provider ids in file order.
    pub fn provider_ids(&self) -> Vec<String> {
        self.providers.iter().map(|(id, _)| id.clone()).collect()
    }

    /// The load error.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Validates one provider config in `models.json` shape, for providers
/// added programmatically; returns Pi's error lines.
pub fn validate_provider_config(id: &str, value: &Value) -> Result<(), String> {
    let mut errors = Errors::default();
    check_provider(&mut errors, &join("providers", id), value);
    if errors.0.is_empty() {
        Ok(())
    } else {
        Err(format!("Invalid provider config:\n{}", errors.0.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comments_and_trailing_commas_outside_strings() {
        assert_eq!(
            strip_json_comments("{\n// c\n\"a\": \"// kept, ]\", // x\n\"b\": [1, 2,],\n}"),
            // Recorded from Pi's `stripJsonComments`.
            "{\n\n\"a\": \"// kept, ]\", \n\"b\": [1, 2]\n}"
        );
        let config = ModelConfig::parse(
            "{ \"providers\": { \"p\": { \"baseUrl\": \"u\", }, }, // tail\n}",
            "f",
        );
        assert_eq!(config.error(), None);
        assert_eq!(config.provider_ids(), vec!["p".to_owned()]);
    }

    // Messages recorded from Pi's `ModelConfig.load` (v1.1.0).
    #[test]
    fn schema_errors_match_pi() {
        let cases = [
            (
                r#"{"providers":{"p":{"models":[{"id":""}]}}}"#,
                "Invalid models.json schema:\n  - providers.p.models.0.id: must not have fewer than 1 characters\n\nFile: f",
            ),
            (
                r#"{"providers":{"p":{"baseUrl":3}}}"#,
                "Invalid models.json schema:\n  - providers.p.baseUrl: must be string\n\nFile: f",
            ),
            (
                r#"{"foo":1}"#,
                "Invalid models.json schema:\n  - providers: must have required properties providers\n\nFile: f",
            ),
            (
                r#"{"providers":{"p":{"models":[{"id":"x","input":["audio"]}]}}}"#,
                "Invalid models.json schema:\n  - providers.p.models.0.input.0: must be equal to constant\n  - providers.p.models.0.input.0: must be equal to constant\n  - providers.p.models.0.input.0: must match a schema in anyOf\n\nFile: f",
            ),
            (
                r#"{"providers":{"p":{"headers":{"a":1}}}}"#,
                "Invalid models.json schema:\n  - providers.p.headers.a: must be string\n\nFile: f",
            ),
        ];
        for (input, expected) in cases {
            let config = ModelConfig::parse(input, "f");
            assert_eq!(config.error(), Some(expected), "{input}");
            assert!(config.provider_ids().is_empty());
        }
        assert!(
            ModelConfig::parse("{", "f")
                .error()
                .is_some_and(|error| error.starts_with("Failed to parse models.json: "))
        );
    }

    #[test]
    fn a_missing_file_is_empty() {
        let config = ModelConfig::load(Path::new("/definitely/not/here/models.json"));
        assert_eq!(config.error(), None);
        assert!(config.provider_ids().is_empty());
    }
}
