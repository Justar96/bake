//! The settings a session reads from `settings.json`.
//!
//! Ported from the read path of Pi
//! `packages/coding-agent/src/core/settings-manager.ts` (v1.1.0), limited to
//! the settings print mode uses: the default provider, model, and thinking
//! level, per-model thinking levels, the queue modes, the session
//! directory, and `blockImages`. Pi reads `~/.pi/agent/settings.json` and
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
//!   does not define, is ignored where Pi would pass it on.
//! - **Errors.** A file that is not a JSON object yields Pi's warning
//!   `Invalid settings file <path>: <message>`; the parser's message is
//!   `serde_json`'s, not V8's.

use std::path::{Path, PathBuf};

use bake_agent::QueueMode;
use bake_ai::ModelThinkingLevel;
use serde_json::{Map, Value};

use crate::auth_storage::{read_text_file, strip_bom};
use crate::model_registry::resolver::parse_thinking_level;
use crate::session::paths::normalize_path;

/// The largest `settings.json` read, in bytes.
pub const MAX_SETTINGS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The settings a session reads.
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// Load errors, as Pi's settings diagnostics word them.
    pub warnings: Vec<String>,
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
            warnings: Vec::new(),
        }
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
}
