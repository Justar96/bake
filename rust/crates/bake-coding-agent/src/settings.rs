//! The settings a session reads from `settings.json`.
//!
//! Ported from the read path of Pi
//! `packages/coding-agent/src/core/settings-manager.ts` (v1.1.0), limited to
//! the settings a session uses: the default provider, model, and thinking
//! level, per-model thinking levels, the queue modes, the session
//! directory, `blockImages`, `images.autoResize`, `defaultTools`, `compaction`
//! (with per-model overrides), `branchSummary.reserveTokens`, and `retry`. Pi reads `~/.pi/agent/settings.json` and
//! merges a project's `.pi/settings.json` over it; Bake reads
//! `$BAKE_HOME/settings.json` only. Bake 0.3's `settings.yaml` beside it is
//! a different file, which this module does not read (D25).
//!
//! # Deviations from Pi
//!
//! - **No project settings.** Project files need Pi's project trust, which
//!   is not ported.
//! - **No writes.** Nothing saves settings.
//! - **Typed reads.** A member of the wrong type, or a thinking level Pi
//!   does not define, is ignored where Pi would pass it on. The compaction
//!   token settings are the exception: they are checked when read and an
//!   invalid one is an error carrying Pi's message, as in Pi.
//! - **Errors.** A file that is not a JSON object yields Pi's warning
//!   `Invalid settings file <path>: <message>`; the parser's message is
//!   `serde_json`'s, not V8's.

use std::path::{Path, PathBuf};

use bake_agent::QueueMode;
use bake_ai::ModelThinkingLevel;
use serde_json::{Map, Value};

use crate::auth_storage::{read_text_file, strip_bom};
use crate::compaction::CompactionSettings;
use crate::model_registry::resolver::parse_thinking_level;
use crate::session::json::{js_number_to_string, js_truthy};
use crate::session::paths::normalize_path;

/// The largest `settings.json` read, in bytes.
pub const MAX_SETTINGS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The settings a session reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// `defaultProvider`.
    pub default_provider: Option<String>,
    /// `defaultModel`.
    pub default_model: Option<String>,
    /// `defaultThinkingLevel`.
    pub default_thinking_level: Option<ModelThinkingLevel>,
    /// `modelThinkingLevels`, keyed `provider/id`, in file order.
    pub model_thinking_levels: Vec<(String, ModelThinkingLevel)>,
    /// `steeringMode` (or the legacy `queueMode`).
    pub steering_mode: QueueMode,
    /// `followUpMode`.
    pub follow_up_mode: QueueMode,
    /// `sessionDir`, normalized.
    pub session_dir: Option<PathBuf>,
    /// `blockImages`.
    pub block_images: bool,
    /// `images.autoResize`; Pi's `getImageAutoResize` defaults it to true
    /// ([`Self::image_auto_resize`]).
    pub images_auto_resize: Option<bool>,
    /// `defaultTools` resolved as Pi's `getDefaultTools` resolves it:
    /// `None` when unset, and string entries only.
    pub default_tools: Option<Vec<String>>,
    /// `compaction` as stored, checked when read (see
    /// [`Self::compaction_settings`]).
    pub compaction: Option<Value>,
    /// `branchSummary.reserveTokens`.
    pub branch_summary_reserve_tokens: u64,
    /// `retry`.
    pub retry: RetrySettings,
    /// Load errors, as Pi's settings diagnostics word them.
    pub warnings: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_provider: None,
            default_model: None,
            default_thinking_level: None,
            model_thinking_levels: Vec::new(),
            steering_mode: QueueMode::default(),
            follow_up_mode: QueueMode::default(),
            session_dir: None,
            block_images: false,
            images_auto_resize: None,
            default_tools: None,
            compaction: None,
            branch_summary_reserve_tokens: DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS,
            retry: RetrySettings::default(),
            warnings: Vec::new(),
        }
    }
}

/// Pi's default `branchSummary.reserveTokens`.
pub const DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS: u64 = 16384;

/// Pi's `getRetrySettings()`: agent-level retry of transient errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySettings {
    /// `retry.enabled`, default true.
    pub enabled: bool,
    /// `retry.maxRetries`, default 3.
    pub max_retries: u32,
    /// `retry.baseDelayMs`, default 2000.
    pub base_delay_ms: u64,
    /// `retry.maxAgentDelayMs`, default 60000.
    pub max_agent_delay_ms: u64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2000,
            max_agent_delay_ms: bake_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS,
        }
    }
}

impl RetrySettings {
    fn from_value(value: Option<&Value>) -> Self {
        let defaults = Self::default();
        let Some(retry) = value.and_then(Value::as_object) else {
            return defaults;
        };
        Self {
            enabled: flag(retry.get("enabled"), defaults.enabled),
            max_retries: retry
                .get("maxRetries")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(defaults.max_retries),
            base_delay_ms: retry
                .get("baseDelayMs")
                .and_then(Value::as_u64)
                .unwrap_or(defaults.base_delay_ms),
            max_agent_delay_ms: retry
                .get("maxAgentDelayMs")
                .and_then(Value::as_u64)
                .unwrap_or(defaults.max_agent_delay_ms),
        }
    }

    /// The policy summarization and `bake-ai`'s retry loop take.
    pub fn policy(&self) -> bake_ai::utils::retry::RetryPolicy {
        bake_ai::utils::retry::RetryPolicy {
            enabled: self.enabled,
            max_retries: self.max_retries,
            base_delay_ms: self.base_delay_ms,
            max_agent_delay_ms: Some(self.max_agent_delay_ms),
        }
    }
}

/// `value ?? fallback`, then JavaScript truthiness.
fn flag(value: Option<&Value>, fallback: bool) -> bool {
    match value {
        None | Some(Value::Null) => fallback,
        value => js_truthy(value),
    }
}

/// JavaScript `String(value)`, as Pi's settings errors print values.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number
            .as_f64()
            .map_or_else(|| number.to_string(), js_number_to_string),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                item => js_string(item),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// A non-negative safe integer, as Pi requires of compaction token counts.
fn token_count(value: &Value) -> Option<u64> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let number = value.as_f64()?;
    (number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number)).then_some(number as u64)
}

fn queue_mode(value: Option<&Value>) -> QueueMode {
    match value.and_then(Value::as_str) {
        Some("all") => QueueMode::All,
        _ => QueueMode::OneAtATime,
    }
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

impl Settings {
    /// Settings from a parsed object, after Pi's `queueMode` migration.
    pub fn from_object(object: &Map<String, Value>) -> Self {
        let steering = object
            .get("steeringMode")
            .or_else(|| object.get("queueMode"));
        Self {
            default_provider: string(object, "defaultProvider"),
            default_model: string(object, "defaultModel"),
            default_thinking_level: object
                .get("defaultThinkingLevel")
                .and_then(Value::as_str)
                .and_then(parse_thinking_level),
            model_thinking_levels: object
                .get("modelThinkingLevels")
                .and_then(Value::as_object)
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|(key, value)| {
                            Some((key.clone(), parse_thinking_level(value.as_str()?)?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            steering_mode: queue_mode(steering),
            follow_up_mode: queue_mode(object.get("followUpMode")),
            session_dir: string(object, "sessionDir").map(|dir| normalize_path(&dir)),
            block_images: object.get("blockImages").and_then(Value::as_bool) == Some(true),
            images_auto_resize: object
                .get("images")
                .and_then(|images| images.get("autoResize"))
                .and_then(Value::as_bool),
            default_tools: object.get("defaultTools").map(|tools| {
                let entries: Vec<String> = tools
                    .as_array()
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                crate::tools::index::resolve_default_tools(&entries)
            }),
            compaction: object.get("compaction").cloned(),
            branch_summary_reserve_tokens: object
                .get("branchSummary")
                .and_then(|summary| summary.get("reserveTokens"))
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS),
            retry: RetrySettings::from_value(object.get("retry")),
            warnings: Vec::new(),
        }
    }

    /// Pi's `getCompactionEnabled`.
    pub fn compaction_enabled(&self) -> bool {
        flag(
            self.compaction
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|compaction| compaction.get("enabled")),
            true,
        )
    }

    /// Pi's `getCompactionTokenSetting`: the model's override, then the
    /// ordinary setting, then the default. An invalid value is an error
    /// with Pi's message.
    fn compaction_token_setting(
        &self,
        field: &str,
        model: Option<(&str, &str)>,
        default: u64,
    ) -> Result<u64, String> {
        let compaction = self.compaction.as_ref().and_then(Value::as_object);
        let ordinary = compaction.and_then(|compaction| compaction.get(field));
        let ordinary = match ordinary {
            None => None,
            Some(value) => Some(token_count(value).ok_or_else(|| {
                format!(
                    "Invalid compaction.{field} setting: {}. Expected a non-negative safe integer.",
                    js_string(value)
                )
            })?),
        };
        let key = model.map(|(provider, id)| format!("{provider}/{id}"));
        let entry = key.as_ref().and_then(|key| {
            compaction
                .and_then(|compaction| compaction.get("modelOverrides"))
                .and_then(Value::as_object)
                .and_then(|overrides| overrides.get(key))
        });
        let mut resolved = None;
        if let (Some(key), Some(entry)) = (&key, entry) {
            let Some(entry) = entry.as_object() else {
                return Err(format!(
                    "Invalid compaction.modelOverrides[\"{key}\"] setting: {}. Expected an object.",
                    js_string(entry)
                ));
            };
            if let Some(value) = entry.get(field) {
                resolved = Some(token_count(value).ok_or_else(|| {
                    format!(
                        "Invalid compaction.modelOverrides[\"{key}\"].{field} setting: {}. Expected a non-negative safe integer.",
                        js_string(value)
                    )
                })?);
            }
        }
        Ok(resolved.or(ordinary).unwrap_or(default))
    }

    /// Pi's `getCompactionSettings(model)`: each token setting resolved
    /// through the model's override (`compaction.modelOverrides`, keyed by
    /// the exact `provider/id`), the ordinary setting, then the default.
    pub fn compaction_settings(
        &self,
        model: Option<(&str, &str)>,
    ) -> Result<CompactionSettings, String> {
        let defaults = crate::compaction::DEFAULT_COMPACTION_SETTINGS;
        Ok(CompactionSettings {
            enabled: self.compaction_enabled(),
            reserve_tokens: self.compaction_token_setting(
                "reserveTokens",
                model,
                defaults.reserve_tokens,
            )?,
            keep_recent_tokens: self.compaction_token_setting(
                "keepRecentTokens",
                model,
                defaults.keep_recent_tokens,
            )?,
        })
    }

    /// Settings from `settings.json` text; `path` names it in warnings.
    pub fn parse(text: &str, path: &Path) -> Self {
        match serde_json::from_str::<Value>(strip_bom(text)) {
            Ok(Value::Object(object)) => Self::from_object(&object),
            Ok(_) => Self::failed(path, "expected a JSON object"),
            Err(error) => Self::failed(path, &error.to_string()),
        }
    }

    fn failed(path: &Path, message: &str) -> Self {
        Self {
            warnings: vec![format!(
                "Invalid settings file {}: {message}",
                path.display()
            )],
            ..Self::default()
        }
    }

    /// The settings of a Bake home: `<home>/settings.json`, or defaults
    /// when it does not exist.
    pub fn load(home: &Path) -> Self {
        let path = home.join("settings.json");
        match read_text_file(&path, MAX_SETTINGS_FILE_BYTES) {
            Ok(None) => Self::default(),
            Ok(Some(text)) => Self::parse(&text, &path),
            Err(error) => Self::failed(&path, &error.to_string()),
        }
    }

    /// Pi's `getImageAutoResize`.
    pub fn image_auto_resize(&self) -> bool {
        self.images_auto_resize.unwrap_or(true)
    }

    /// Pi's `getModelThinkingLevel`.
    pub fn model_thinking_level(&self, provider: &str, id: &str) -> Option<ModelThinkingLevel> {
        let key = format!("{provider}/{id}");
        self.model_thinking_levels
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, level)| *level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_session_settings() {
        let settings = Settings::parse(
            r#"{"defaultProvider":"p","defaultModel":"m","defaultThinkingLevel":"high",
                "modelThinkingLevels":{"p/m":"low","p/x":"bogus"},"queueMode":"all",
                "followUpMode":"one-at-a-time","blockImages":true}"#,
            Path::new("s.json"),
        );
        assert_eq!(settings.default_provider.as_deref(), Some("p"));
        assert_eq!(settings.default_model.as_deref(), Some("m"));
        assert_eq!(
            settings.default_thinking_level,
            Some(ModelThinkingLevel::High)
        );
        assert_eq!(
            settings.model_thinking_level("p", "m"),
            Some(ModelThinkingLevel::Low)
        );
        assert_eq!(settings.model_thinking_level("p", "x"), None);
        assert_eq!(settings.steering_mode, QueueMode::All);
        assert_eq!(settings.follow_up_mode, QueueMode::OneAtATime);
        assert!(settings.block_images);
        assert!(settings.warnings.is_empty());
        assert!(settings.image_auto_resize());
        assert_eq!(settings.default_tools, None);
    }

    // Pi `default-tools-setting.test.ts` and `getImageAutoResize`.
    #[test]
    fn reads_the_tool_and_image_settings() {
        let settings = Settings::parse(
            r#"{"images":{"autoResize":false},"defaultTools":["+grep",5,"-write"]}"#,
            Path::new("s.json"),
        );
        assert!(!settings.image_auto_resize());
        assert_eq!(
            settings.default_tools.as_deref(),
            Some(&["read", "bash", "edit", "grep"].map(String::from)[..])
        );
        let settings = Settings::parse(r#"{"defaultTools":"read"}"#, Path::new("s.json"));
        assert_eq!(settings.default_tools, Some(Vec::new()));
    }

    #[test]
    fn a_broken_file_warns_and_defaults() {
        let settings = Settings::parse("{", Path::new("s.json"));
        assert_eq!(settings.default_provider, None);
        assert_eq!(settings.warnings.len(), 1);
        assert!(settings.warnings[0].starts_with("Invalid settings file s.json: "));
        assert_eq!(
            Settings::load(Path::new("/definitely/not/a/bake/home")),
            Settings::default()
        );
    }

    // Ports of Pi `test/settings-manager-compaction.test.ts` (#8133). Not
    // ported: the project-settings merge and reload case and the saved
    // toggle case (Bake reads no project settings and writes none), and the
    // non-finite runtime values, which JSON cannot hold.
    mod compaction_model_overrides {
        use super::*;
        use serde_json::json;

        const MODEL: (&str, &str) = ("provider", "family/model");
        const KEY: &str = "provider/family/model";

        fn defaults() -> CompactionSettings {
            crate::compaction::DEFAULT_COMPACTION_SETTINGS
        }

        fn settings(value: Value) -> Settings {
            Settings::from_object(value.as_object().unwrap_or(&Map::new()))
        }

        // Pi: "uses defaults without compaction settings".
        #[test]
        fn defaults_without_settings() {
            let manager = Settings::default();
            assert_eq!(manager.compaction_settings(None), Ok(defaults()));
            assert_eq!(manager.compaction_settings(Some(MODEL)), Ok(defaults()));
        }

        // Pi: "resolves each field independently and keeps individual
        // getters consistent" (without the runtime overrides).
        #[test]
        fn fields_resolve_independently() {
            let manager = settings(json!({ "compaction": {
                "reserveTokens": 8192, "keepRecentTokens": 10000,
                "modelOverrides": { KEY: { "reserveTokens": 400000 } },
            }}));
            assert_eq!(
                manager.compaction_settings(Some(MODEL)),
                Ok(CompactionSettings {
                    enabled: true,
                    reserve_tokens: 400000,
                    keep_recent_tokens: 10000
                })
            );
            assert_eq!(
                manager.compaction_settings(None),
                Ok(CompactionSettings {
                    enabled: true,
                    reserve_tokens: 8192,
                    keep_recent_tokens: 10000
                })
            );
        }

        // Pi: "falls back to built-in defaults for missing fields".
        #[test]
        fn missing_fields_fall_back() {
            let manager = settings(json!({ "compaction": {
                "modelOverrides": { KEY: { "keepRecentTokens": 1024 } },
            }}));
            assert_eq!(
                manager.compaction_settings(Some(MODEL)),
                Ok(CompactionSettings {
                    keep_recent_tokens: 1024,
                    ..defaults()
                })
            );
        }

        // Pi: "matches exact provider/model IDs, including IDs containing
        // slashes".
        #[test]
        fn overrides_match_exact_ids() {
            let manager = settings(json!({ "compaction": { "modelOverrides": {
                KEY: { "reserveTokens": 400000 },
                "provider/*": { "reserveTokens": 1 },
                "family/model": { "reserveTokens": 2 },
            }}}));
            assert_eq!(
                manager
                    .compaction_settings(Some(MODEL))
                    .map(|s| s.reserve_tokens),
                Ok(400000)
            );
            for other in [
                ("other", "family/model"),
                ("provider", "other"),
                ("provider", "family/Model"),
            ] {
                assert_eq!(manager.compaction_settings(Some(other)), Ok(defaults()));
            }
        }

        // Pi: "keeps enabled global ..." (the read half): `enabled` in an
        // override is not a model setting.
        #[test]
        fn enabled_is_not_overridden_per_model() {
            let manager = settings(json!({ "compaction": {
                "modelOverrides": { KEY: { "enabled": false, "reserveTokens": 400000 } },
            }}));
            assert!(
                manager
                    .compaction_settings(Some(MODEL))
                    .is_ok_and(|s| s.enabled)
            );
            let disabled = settings(json!({ "compaction": { "enabled": false } }));
            assert!(disabled.compaction_settings(None).is_ok_and(|s| !s.enabled));
        }

        fn invalid_values() -> Vec<(Value, &'static str)> {
            vec![
                (json!(null), "null"),
                (json!(-1), "-1"),
                (json!(1.5), "1.5"),
                (json!("400000"), "400000"),
                (json!(true), "true"),
                (json!({}), "[object Object]"),
                (json!([]), ""),
                (json!(9007199254740992u64), "9007199254740992"),
            ]
        }

        // Pi: "model override %s" / "reports invalid token values".
        #[test]
        fn invalid_override_values_are_reported() {
            for field in ["reserveTokens", "keepRecentTokens"] {
                for (value, printed) in invalid_values() {
                    let manager = settings(json!({ "compaction": {
                        "modelOverrides": { KEY: { field: value } },
                    }}));
                    assert_eq!(
                        manager.compaction_settings(Some(MODEL)),
                        Err(format!(
                            "Invalid compaction.modelOverrides[\"{KEY}\"].{field} setting: {printed}. Expected a non-negative safe integer."
                        ))
                    );
                    assert_eq!(manager.compaction_settings(None), Ok(defaults()));
                    assert_eq!(
                        manager.compaction_settings(Some(("other", MODEL.1))),
                        Ok(defaults())
                    );
                }
            }
        }

        // Pi: "ordinary compaction.%s" / "reports invalid values even when a
        // valid model override exists".
        #[test]
        fn invalid_ordinary_values_are_reported() {
            for field in ["reserveTokens", "keepRecentTokens"] {
                for (value, printed) in invalid_values() {
                    let manager = settings(json!({ "compaction": {
                        field: value,
                        "modelOverrides": { KEY: { field: 4096 } },
                    }}));
                    let error = format!(
                        "Invalid compaction.{field} setting: {printed}. Expected a non-negative safe integer."
                    );
                    assert_eq!(manager.compaction_settings(None), Err(error.clone()));
                    assert_eq!(manager.compaction_settings(Some(MODEL)), Err(error));
                }
            }
        }

        // Pi: "reports malformed model entries".
        #[test]
        fn malformed_entries_are_reported() {
            for (entry, printed) in [
                (json!(null), "null"),
                (json!(false), "false"),
                (json!(42), "42"),
                (json!("invalid"), "invalid"),
                (json!([]), ""),
            ] {
                let manager =
                    settings(json!({ "compaction": { "modelOverrides": { KEY: entry } } }));
                assert_eq!(
                    manager.compaction_settings(Some(MODEL)),
                    Err(format!(
                        "Invalid compaction.modelOverrides[\"{KEY}\"] setting: {printed}. Expected an object."
                    ))
                );
            }
        }

        // Pi: "accepts zero in ordinary settings and model overrides".
        #[test]
        fn zero_is_accepted() {
            let zero = CompactionSettings {
                enabled: true,
                reserve_tokens: 0,
                keep_recent_tokens: 0,
            };
            let manager =
                settings(json!({ "compaction": { "reserveTokens": 0, "keepRecentTokens": 0 } }));
            assert_eq!(manager.compaction_settings(Some(MODEL)), Ok(zero));
            let manager = settings(json!({ "compaction": {
                "reserveTokens": 1000, "keepRecentTokens": 1000,
                "modelOverrides": { KEY: { "reserveTokens": 0, "keepRecentTokens": 0 } },
            }}));
            assert_eq!(manager.compaction_settings(Some(MODEL)), Ok(zero));
        }

        #[test]
        fn retry_and_branch_summary_settings_default_like_pi() {
            let manager = Settings::default();
            assert_eq!(
                manager.retry,
                RetrySettings {
                    enabled: true,
                    max_retries: 3,
                    base_delay_ms: 2000,
                    max_agent_delay_ms: 60000
                }
            );
            assert_eq!(manager.branch_summary_reserve_tokens, 16384);
            let manager = settings(json!({
                "retry": { "enabled": false, "maxRetries": 5, "baseDelayMs": 1, "maxAgentDelayMs": 5 },
                "branchSummary": { "reserveTokens": 100 },
            }));
            assert_eq!(
                manager.retry,
                RetrySettings {
                    enabled: false,
                    max_retries: 5,
                    base_delay_ms: 1,
                    max_agent_delay_ms: 5
                }
            );
            assert_eq!(manager.branch_summary_reserve_tokens, 100);
        }
    }
}
