//! The settings a session reads from `settings.json`.
//!
//! Ported from the read path of Pi
//! `packages/coding-agent/src/core/settings-manager.ts` (v1.1.0), limited to
//! the settings a session uses: the default provider, model, and thinking
//! level, per-model thinking levels, the queue modes, the session
//! directory, `blockImages`, `images.autoResize`, `defaultTools`, `compaction`
//! (with per-model overrides, and Bake's own `compaction.thresholdPercent`
//! and `compaction.cachedSummary`), `branchSummary.reserveTokens` and
//! `branchSummary.skipPrompt`, `retry`, Pi v1.3.0's `codemode.mode` and
//! `codemode.inlineBudget` ([`crate::tools::codemode`]), Pi v1.4.0's
//! `terminal.showTerminalProgress`, Pi v1.5.2's `markdown.mermaid`
//! ([`MermaidRendering`]),
//! and Bake's own `permission` section ([`crate::permissions`]),
//! `terminal.notifications`, `statusLine` (the status line's optional
//! readings), `webSearch.model` ([`crate::web::search`]), and
//! `maxToolArgumentWhitespace` (Bake's route option). Pi reads `~/.pi/agent/settings.json` and
//! merges a project's `.pi/settings.json` over it; Bake reads
//! `$BAKE_HOME/settings.json` only.
//!
//! [`SettingsStore`] writes the file, porting the global-scope saves of
//! `SettingsManager` (Pi v1.2.0, `06a466935536f8c9bcacb2d5efc0fc021be1bdff`)
//! for the settings above that Pi has setters for (with Pi v1.5.2's
//! `setMermaidRenderingMode`), and for the compaction members and
//! `branchSummary.skipPrompt` that `/settings` changes, through
//! the locked atomic backend of [`crate::json_store`].
//!
//! # Deviations from Pi
//!
//! - **No project settings.** Project files need Pi's project trust, which
//!   is not ported.
//! - **Writes are patches.** Pi's manager records which fields a session
//!   changed and, when saving, copies those fields from its in-memory copy
//!   over the file read under the lock. [`SettingsStore`] keeps no copy: each
//!   setter changes its own members of the file read under the lock and
//!   returns the settings now stored. A model's thinking level changes only
//!   that model's entry of `modelThinkingLevels`, where Pi rewrites the whole
//!   map from memory. Writes are synchronous and return their error instead
//!   of Pi's write queue and `drainErrors`.
//! - **Writes refuse a broken file.** Pi skips saving when the file failed
//!   to load; a setter returns [`StoreError::Refused`] and leaves the file's
//!   bytes as they are. Pi's legacy-key migrations (`queueMode` and others)
//!   apply when reading but are not written back, so a save leaves members it
//!   does not set as they were.
//! - **Typed reads.** A member of the wrong type, or a thinking level Pi
//!   does not define, is ignored where Pi would pass it on. The compaction
//!   token settings are the exception: they are checked when read and an
//!   invalid one is an error carrying Pi's message, as in Pi.
//!   `compaction.thresholdPercent` is checked the same way.
//! - **Errors.** A file that is not a JSON object yields Pi's warning
//!   `Invalid settings file <path>: <message>`; the parser's message is
//!   `serde_json`'s, not V8's.

use std::path::{Path, PathBuf};

use bake_agent::QueueMode;
use bake_ai::ModelThinkingLevel;
use serde_json::{Map, Value};

use crate::auth_storage::{read_text_file, strip_bom};
use crate::compaction::CompactionSettings;
use crate::json_store::{JsonFileStore, StoreError, remove_member, set_member};
use crate::model_registry::resolver::parse_thinking_level;
use crate::session::json::{js_number_to_string, js_truthy};
use crate::session::paths::normalize_path;
use crate::tools::codemode::CodemodeMode;

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
    /// `enabledModels`, Pi's `getEnabledModels`: model scope patterns when
    /// `--models` is not passed.
    pub enabled_models: Option<Vec<String>>,
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
    /// `defaultTools` as written, string entries only, and `None` when
    /// unset. The CLI resolves it over Bake's defaults
    /// ([`crate::tools::index::resolve_default_tools_over`]): names replace
    /// them, and `+name` and `-name` entries change them.
    pub default_tools: Option<Vec<String>>,
    /// `maxConcurrentReadTools` as stored, checked by
    /// [`Self::read_tool_concurrency`]. Absent preserves unlimited batches.
    pub max_concurrent_read_tools: Option<Value>,
    /// `bash.foregroundSeconds` as stored, checked by
    /// [`Self::bash_foreground_budget`]. Bake's own.
    pub bash_foreground_seconds: Option<Value>,
    /// `nestedContextFiles` as stored, checked by
    /// [`Self::nested_context_files`]. Bake's own.
    pub nested_context_files: Option<Value>,
    /// Set by the caller when context files are not loaded for the run
    /// (`-nc`): no file is then found while working either. Not a
    /// `settings.json` member.
    pub no_context_files: bool,
    /// `skills.listingTokens` as stored, checked by
    /// [`Self::skills_listing_tokens`]. Bake's own.
    pub skills_listing_tokens: Option<Value>,
    /// `modelProfiles` as stored, checked by [`Self::model_profiles`].
    /// Bake's own.
    pub model_profiles: Option<Value>,
    /// Set by the caller when the active tools were chosen explicitly, as
    /// `--tools` chooses them: the profiles' `tools` members then change
    /// nothing ([`Self::model_profile`]). Not a `settings.json` member.
    pub explicit_tools: bool,
    /// `taskDocuments`: enables the versioned task document tool and context.
    pub task_documents: bool,
    /// `subagents`: gives a top-level session the subagent tools
    /// ([`crate::subagents`]). Defaults to false.
    pub subagents: bool,
    /// `telecom`: joins the session to the other Bake sessions working in
    /// its repository and gives it the `telecom` tool ([`crate::telecom`]).
    /// Defaults to false, while its effect on models is unmeasured;
    /// `BAKE_TELECOM` overrides it for a run ([`crate::telecom::enabled`]).
    pub telecom: bool,
    /// `visualize`: makes the `visualize` tool one of an interactive
    /// session's defaults ([`crate::tools::visualize`]). Defaults to true,
    /// by the owner's decision of 2026-11-14; `false` leaves it opt-in.
    pub visualize: bool,
    /// `schedules`: gives a top-level session the reminder tools and their
    /// timer ([`crate::schedules`]). Defaults to false.
    pub schedules: bool,
    /// `hooks`: the Claude Code and Codex hooks files to run
    /// ([`crate::hooks`]). Bake's own member; unset runs no hook.
    pub hooks: HookSettings,
    /// `compaction` as stored, checked when read (see
    /// [`Self::compaction_settings`]).
    pub compaction: Option<Value>,
    /// `backgroundCompaction`: opt-in speculative summaries before the
    /// ordinary foreground compaction threshold. Defaults to false.
    pub background_compaction: bool,
    /// `contextMasking`: experimental persisted masking of old observations,
    /// retaining ordinary foreground compaction as the fallback. Default false.
    pub context_masking: bool,
    /// `branchSummary.reserveTokens`.
    pub branch_summary_reserve_tokens: u64,
    /// `branchSummary.skipPrompt`: `/tree` navigates without asking whether
    /// to summarize the branch it leaves. Defaults to false.
    pub branch_summary_skip_prompt: bool,
    /// `retry`.
    pub retry: RetrySettings,
    /// `permission` as stored (`presets` and `defaultPreset`), read by
    /// [`crate::permissions`]. Bake's own section, not Pi's.
    pub permission: Option<Value>,
    /// `maxToolArgumentWhitespace`: Bake's bound on the whitespace a streamed
    /// tool call's arguments may end with (`config.ts`, a positive integer);
    /// `None` uses [`bake_ai::DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE`].
    pub max_tool_argument_whitespace: Option<usize>,
    /// `webSearch.model`: Bake's own, the CLIProxyAPI route model
    /// `web_search` runs on ([`crate::web::search::select_search_model`]);
    /// `None` selects one.
    pub web_search_model: Option<String>,
    /// `cliproxyapi`: Bake's own member, the CLIProxyAPI route a Rust
    /// login wrote, or `null` after a logout (see [`CliProxySetting`]).
    pub cliproxyapi: CliProxySetting,
    /// `codemode.mode`, Pi v1.3.0's: how the active `codemode` tool presents
    /// the tools scripts can call. `only` is read as `only`, anything else
    /// as the default, `on`.
    pub codemode_mode: CodemodeMode,
    /// `codemode.inlineBudget`, Pi v1.3.0's: the estimated tokens the tool
    /// declarations in the `codemode` description may take. `None` when
    /// unset or not a finite number of at least 0, which uses
    /// [`crate::tools::codemode::DEFAULT_CODEMODE_INLINE_BUDGET`].
    pub codemode_inline_budget: Option<f64>,
    /// `terminal.showTerminalProgress`, Pi v1.4.0's: whether the terminal
    /// shows an OSC 9;4 progress indicator while a turn runs. `None` when
    /// unset or not a boolean; the terminal interface then shows it in
    /// Ghostty only.
    pub show_terminal_progress: Option<bool>,
    /// `terminal.notifications`, Bake's own: whether the terminal shows a
    /// desktop notification when a turn ends or asks for approval while
    /// it is out of focus. `None` when unset or not a boolean; the
    /// terminal interface then shows them in Ghostty only.
    pub terminal_notifications: Option<bool>,
    /// `statusLine`, Bake's own: the status line's optional readings.
    pub status_line: StatusLineSettings,
    /// `markdown.mermaid`, Pi v1.5.2's: how the interactive terminal draws
    /// Mermaid fences in answers.
    pub mermaid: MermaidRendering,
    /// Load errors, as Pi's settings diagnostics word them.
    pub warnings: Vec<String>,
}

/// The `cliproxyapi` member of `settings.json`, which `bake` owns: a
/// login writes the route there, a logout writes `null`, and a refresh
/// replaces its `models`.
#[derive(Clone, Default, PartialEq)]
pub enum CliProxySetting {
    /// No member: never signed in.
    #[default]
    Absent,
    /// `null`: signed out; no route.
    SignedOut,
    /// An object: the route as `CliProxyRoute::to_settings_json` writes it.
    /// It is checked when it is used.
    Route(Value),
    /// Any other value: no route, with a warning.
    Invalid,
}

impl std::fmt::Debug for CliProxySetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => f.write_str("Absent"),
            Self::SignedOut => f.write_str("SignedOut"),
            Self::Route(route) => f.debug_tuple("Route").field(&RouteShape(route)).finish(),
            Self::Invalid => f.write_str("Invalid"),
        }
    }
}

/// A saved route's shape for `Debug`: its member names and how many
/// models it lists. Values stay out, since a route may hold header
/// values.
pub(crate) struct RouteShape<'a>(pub &'a Value);

impl std::fmt::Debug for RouteShape<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let members: Vec<&str> = self
            .0
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let models = self.0.get("models").and_then(Value::as_array).map(Vec::len);
        f.debug_struct("Route")
            .field("members", &members)
            .field("models", &models)
            .finish()
    }
}

impl CliProxySetting {
    fn from_value(value: Option<&Value>) -> Self {
        match value {
            None => Self::Absent,
            Some(Value::Null) => Self::SignedOut,
            Some(route @ Value::Object(_)) => Self::Route(route.clone()),
            Some(_) => Self::Invalid,
        }
    }
}

/// The `settings.json` member [`CliProxySetting`] reads.
pub const CLIPROXYAPI_MEMBER: &str = "cliproxyapi";

/// Pi's `setCompactionEnabled` on a settings value: `compaction.enabled`,
/// keeping the section's other members.
pub fn set_compaction_enabled_in(settings: &mut Settings, enabled: bool) {
    set_compaction_member_in(settings, "enabled", Some(Value::Bool(enabled)));
}

/// Sets the `compaction` member `key` of a settings value to `value`, or
/// removes it when `value` is `None`, keeping the section's other members.
/// A `compaction` that is not an object is replaced.
pub fn set_compaction_member_in(settings: &mut Settings, key: &str, value: Option<Value>) {
    let mut compaction = settings
        .compaction
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    match value {
        Some(value) => {
            compaction.insert(key.into(), value);
        }
        None => {
            compaction.shift_remove(key);
        }
    }
    settings.compaction = Some(Value::Object(compaction));
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_provider: None,
            default_model: None,
            default_thinking_level: None,
            enabled_models: None,
            model_thinking_levels: Vec::new(),
            steering_mode: QueueMode::default(),
            follow_up_mode: QueueMode::default(),
            session_dir: None,
            block_images: false,
            images_auto_resize: None,
            default_tools: None,
            max_concurrent_read_tools: None,
            bash_foreground_seconds: None,
            skills_listing_tokens: None,
            nested_context_files: None,
            no_context_files: false,
            model_profiles: None,
            explicit_tools: false,
            task_documents: false,
            subagents: false,
            telecom: false,
            visualize: true,
            schedules: false,
            hooks: HookSettings::default(),
            compaction: None,
            background_compaction: false,
            context_masking: false,
            branch_summary_reserve_tokens: DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS,
            branch_summary_skip_prompt: false,
            retry: RetrySettings::default(),
            permission: None,
            max_tool_argument_whitespace: None,
            web_search_model: None,
            cliproxyapi: CliProxySetting::Absent,
            codemode_mode: CodemodeMode::On,
            codemode_inline_budget: None,
            show_terminal_progress: None,
            terminal_notifications: None,
            status_line: StatusLineSettings::default(),
            mermaid: MermaidRendering::default(),
            warnings: Vec::new(),
        }
    }
}

/// `markdown.mermaid`, Pi v1.5.2's `MermaidRenderingMode`: whether the
/// interactive terminal draws ```` ```mermaid ```` fences in answers as
/// diagrams.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MermaidRendering {
    /// `off`: fences draw as code.
    Off,
    /// `final`: a fence draws as a diagram once it is complete.
    Final,
    /// `streaming`, the default: also while it is being written.
    #[default]
    Streaming,
}

impl MermaidRendering {
    /// Pi's `getMermaidRenderingMode`: `off` and `final` as written, and
    /// anything else, unset included, the default.
    pub fn from_value(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("off") => Self::Off,
            Some("final") => Self::Final,
            _ => Self::Streaming,
        }
    }

    /// The value as `settings.json` holds it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Final => "final",
            Self::Streaming => "streaming",
        }
    }
}

/// The `statusLine` member of `settings.json`: which of the status line's
/// optional readings the interactive terminal shows, after Pi's footer.
/// Each is off unless `true`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusLineSettings {
    /// `statusLine.tokens`: the session's input and output token totals.
    pub tokens: bool,
    /// `statusLine.cache`: the session's cache reads and writes, and the
    /// share of the latest prompt read from the cache.
    pub cache: bool,
}

impl StatusLineSettings {
    fn from_value(value: Option<&Value>) -> Self {
        let on = |key: &str| {
            value
                .and_then(|line| line.get(key))
                .and_then(Value::as_bool)
                == Some(true)
        };
        Self {
            tokens: on("tokens"),
            cache: on("cache"),
        }
    }

    /// Whether any reading needs the session's usage totals.
    pub fn any(&self) -> bool {
        self.tokens || self.cache
    }
}

/// The `hooks` member of `settings.json`: the path of a Claude Code
/// `hooks.json` (or a Claude Code settings file whose `hooks` member holds
/// one) under `claudeCode`, and a Codex `hooks.json` under `codex`. A path
/// may start with `~/`; a relative one is read from the Bake home
/// ([`crate::hooks::home`]). An empty or non-string value is unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookSettings {
    /// `hooks.claudeCode`.
    pub claude_code: Option<String>,
    /// `hooks.codex`.
    pub codex: Option<String>,
}

impl HookSettings {
    fn from_value(value: Option<&Value>) -> Self {
        let Some(object) = value.and_then(Value::as_object) else {
            return Self::default();
        };
        Self {
            claude_code: string(object, "claudeCode"),
            codex: string(object, "codex"),
        }
    }

    /// Whether a file is named.
    pub fn is_set(&self) -> bool {
        self.claude_code.is_some() || self.codex.is_some()
    }
}

/// Pi's default `branchSummary.reserveTokens`.
pub const DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS: u64 = 16384;

#[cfg(test)]
mod read_concurrency_tests {
    use super::*;

    #[test]
    fn explicit_read_concurrency_is_bounded_and_checked() {
        assert_eq!(Settings::default().read_tool_concurrency().unwrap(), None);
        for value in [
            serde_json::json!(1),
            serde_json::json!(4),
            serde_json::json!(64),
        ] {
            let settings = Settings::from_object(
                serde_json::json!({"maxConcurrentReadTools":value})
                    .as_object()
                    .unwrap(),
            );
            assert_eq!(
                settings.read_tool_concurrency().unwrap().unwrap().get() as u64,
                value.as_u64().unwrap()
            );
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!(65),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("4"),
            serde_json::json!(null),
            serde_json::json!(true),
        ] {
            let settings = Settings::from_object(
                serde_json::json!({"maxConcurrentReadTools":value})
                    .as_object()
                    .unwrap(),
            );
            assert!(settings.read_tool_concurrency().is_err(), "{value}");
        }
    }

    // `bash.foregroundSeconds`: 300 when unset, any positive integer, and
    // anything else an error.
    #[test]
    fn the_foreground_budget_is_a_positive_integer() {
        let budget = |value: serde_json::Value| {
            Settings::from_object(
                serde_json::json!({ "bash": { "foregroundSeconds": value } })
                    .as_object()
                    .unwrap(),
            )
            .bash_foreground_budget()
        };
        assert_eq!(
            Settings::default().bash_foreground_budget(),
            Ok(std::time::Duration::from_secs(300))
        );
        let unrelated = Settings::from_object(
            serde_json::json!({ "bash": { "other": 1 } })
                .as_object()
                .unwrap(),
        );
        assert_eq!(
            unrelated.bash_foreground_budget(),
            Ok(std::time::Duration::from_secs(300))
        );
        for seconds in [1_u64, 45, 86_400] {
            assert_eq!(
                budget(serde_json::json!(seconds)),
                Ok(std::time::Duration::from_secs(seconds))
            );
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!(-5),
            serde_json::json!(2.5),
            serde_json::json!("60"),
            serde_json::json!(null),
            serde_json::json!(true),
        ] {
            assert_eq!(
                budget(value.clone()),
                Err("Invalid bash.foregroundSeconds setting: expected a positive integer.".into()),
                "{value}"
            );
        }
    }
}

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

/// A boolean member of the `terminal` section; `None` when it is absent or
/// not a boolean.
fn terminal_flag(object: &Map<String, Value>, key: &str) -> Option<bool> {
    object
        .get("terminal")
        .and_then(|terminal| terminal.get(key))
        .and_then(Value::as_bool)
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
            enabled_models: object
                .get("enabledModels")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                }),
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
                tools
                    .as_array()
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            }),
            task_documents: object.get("taskDocuments").and_then(Value::as_bool) == Some(true),
            subagents: object.get("subagents").and_then(Value::as_bool) == Some(true),
            telecom: object.get("telecom").and_then(Value::as_bool) == Some(true),
            visualize: object.get("visualize").and_then(Value::as_bool) != Some(false),
            schedules: object.get("schedules").and_then(Value::as_bool) == Some(true),
            hooks: HookSettings::from_value(object.get("hooks")),
            max_concurrent_read_tools: object.get("maxConcurrentReadTools").cloned(),
            bash_foreground_seconds: object
                .get("bash")
                .and_then(|bash| bash.get("foregroundSeconds"))
                .cloned(),
            skills_listing_tokens: object
                .get("skills")
                .and_then(|skills| skills.get("listingTokens"))
                .cloned(),
            nested_context_files: object.get("nestedContextFiles").cloned(),
            no_context_files: false,
            model_profiles: object.get("modelProfiles").cloned(),
            explicit_tools: false,
            compaction: object.get("compaction").cloned(),
            background_compaction: object.get("backgroundCompaction").and_then(Value::as_bool)
                == Some(true),
            context_masking: object.get("contextMasking").and_then(Value::as_bool) == Some(true),
            branch_summary_reserve_tokens: object
                .get("branchSummary")
                .and_then(|summary| summary.get("reserveTokens"))
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS),
            branch_summary_skip_prompt: object
                .get("branchSummary")
                .and_then(|summary| summary.get("skipPrompt"))
                .and_then(Value::as_bool)
                == Some(true),
            retry: RetrySettings::from_value(object.get("retry")),
            permission: object.get("permission").cloned(),
            max_tool_argument_whitespace: object
                .get("maxToolArgumentWhitespace")
                .and_then(Value::as_u64)
                .filter(|max| *max >= 1)
                .and_then(|max| usize::try_from(max).ok()),
            web_search_model: object
                .get("webSearch")
                .and_then(Value::as_object)
                .and_then(|search| string(search, "model")),
            cliproxyapi: CliProxySetting::from_value(object.get(CLIPROXYAPI_MEMBER)),
            // Pi's `readMode` and `readInlineBudget`.
            codemode_mode: match object
                .get("codemode")
                .and_then(|codemode| codemode.get("mode"))
                .and_then(Value::as_str)
            {
                Some("only") => CodemodeMode::Only,
                _ => CodemodeMode::On,
            },
            codemode_inline_budget: object
                .get("codemode")
                .and_then(|codemode| codemode.get("inlineBudget"))
                .and_then(Value::as_f64)
                .filter(|budget| budget.is_finite() && *budget >= 0.0),
            show_terminal_progress: terminal_flag(object, "showTerminalProgress"),
            terminal_notifications: terminal_flag(object, "notifications"),
            status_line: StatusLineSettings::from_value(object.get("statusLine")),
            mermaid: MermaidRendering::from_value(
                object
                    .get("markdown")
                    .and_then(|markdown| markdown.get("mermaid")),
            ),
            warnings: Vec::new(),
        }
    }

    /// Optional bound for trusted native read batches. Invalid explicit
    /// values fail session creation instead of silently removing the bound.
    pub fn read_tool_concurrency(&self) -> Result<Option<std::num::NonZeroUsize>, String> {
        self.max_concurrent_read_tools
            .as_ref()
            .map(|value| {
                value
                    .as_u64()
                    .filter(|value| (1..=64).contains(value))
                    .and_then(|value| std::num::NonZeroUsize::new(value as usize))
                    .ok_or_else(|| {
                        "Invalid maxConcurrentReadTools setting: expected an integer from 1 to 64."
                            .to_owned()
                    })
            })
            .transpose()
    }

    /// How long a `bash` call without `timeout` waits before its command
    /// continues as a background job ([`crate::tools::bash::BashToolOptions::foreground_budget`]):
    /// `bash.foregroundSeconds`, a positive integer, else
    /// [`crate::tools::bash::DEFAULT_FOREGROUND_SECONDS`]. An invalid
    /// explicit value fails session creation, as
    /// [`Self::read_tool_concurrency`]'s does.
    pub fn bash_foreground_budget(&self) -> Result<std::time::Duration, String> {
        match &self.bash_foreground_seconds {
            None => Ok(std::time::Duration::from_secs(
                crate::tools::bash::DEFAULT_FOREGROUND_SECONDS,
            )),
            Some(value) => value
                .as_u64()
                .filter(|seconds| *seconds >= 1)
                .map(std::time::Duration::from_secs)
                .ok_or_else(|| {
                    "Invalid bash.foregroundSeconds setting: expected a positive integer."
                        .to_owned()
                }),
        }
    }

    /// Whether context files found while working join the prompt
    /// (`crate::agent_session` adds them): `nestedContextFiles`, a boolean,
    /// `false` when unset. An invalid explicit value fails session
    /// creation, as [`Self::bash_foreground_budget`]'s does.
    pub fn nested_context_files(&self) -> Result<bool, String> {
        match &self.nested_context_files {
            None => Ok(false),
            Some(value) => value.as_bool().ok_or_else(|| {
                "Invalid nestedContextFiles setting: expected true or false.".to_owned()
            }),
        }
    }

    /// The estimated tokens the system prompt's skills list may take
    /// ([`crate::skills::format_skills_for_prompt`]): `skills.listingTokens`,
    /// a positive integer, else [`crate::skills::DEFAULT_LISTING_TOKENS`].
    /// An invalid explicit value fails session creation, as
    /// [`Self::bash_foreground_budget`]'s does.
    pub fn skills_listing_tokens(&self) -> Result<u64, String> {
        match &self.skills_listing_tokens {
            None => Ok(crate::skills::DEFAULT_LISTING_TOKENS),
            Some(value) => value.as_u64().filter(|tokens| *tokens >= 1).ok_or_else(|| {
                "Invalid skills.listingTokens setting: expected a positive integer.".to_owned()
            }),
        }
    }

    /// `modelProfiles`, checked: each entry's pattern and profile, in the
    /// order written. Any malformed entry is an error naming its key and
    /// member, and session creation stops with it, as an invalid
    /// `permission` section stops it; nothing is half-applied. Tool names
    /// are checked by the caller that knows the tools
    /// ([`ModelProfile::unknown_tool`]).
    pub fn model_profiles(&self) -> Result<Vec<(String, ModelProfile)>, String> {
        let Some(value) = &self.model_profiles else {
            return Ok(Vec::new());
        };
        let Some(entries) = value.as_object() else {
            return Err(format!(
                "Invalid modelProfiles setting: {}. Expected an object of model patterns.",
                js_string(value)
            ));
        };
        entries
            .iter()
            .map(|(key, entry)| Ok((key.clone(), ModelProfile::parse(key, entry)?)))
            .collect()
    }

    /// The profile of `model`: every `modelProfiles` entry whose pattern
    /// matches it, as `enabledModels` matches, in the order written, a later
    /// entry overriding an earlier one member by member and `tools` lists
    /// joined in order. Empty when none matches. Malformed entries, which
    /// session creation refuses, are ignored here.
    pub fn model_profile(&self, model: &bake_ai::Model) -> ModelProfile {
        let mut profile = ModelProfile::default();
        for (pattern, entry) in self.model_profiles().unwrap_or_default() {
            if crate::model_registry::resolver::model_matches_pattern(&pattern, model) {
                profile.overlay(entry);
            }
        }
        if self.explicit_tools {
            profile.tools.clear();
        }
        profile
    }

    /// The thinking level `model`'s settings choose: its profile's
    /// `thinkingLevel`, else its `modelThinkingLevels` entry.
    pub fn thinking_level_for(&self, model: &bake_ai::Model) -> Option<ModelThinkingLevel> {
        self.model_profile(model)
            .thinking_level
            .or_else(|| self.model_thinking_level(&model.provider, &model.id))
    }

    /// [`Self::compaction_settings`] for `model`, with its profile's
    /// `compaction` members over `compaction.modelOverrides` and the
    /// ordinary settings.
    pub fn compaction_settings_for(
        &self,
        model: Option<&bake_ai::Model>,
    ) -> Result<CompactionSettings, String> {
        let mut settings = self
            .compaction_settings(model.map(|model| (model.provider.as_str(), model.id.as_str())))?;
        if let Some(model) = model {
            let profile = self.model_profile(model);
            if let Some(tokens) = profile.reserve_tokens {
                settings.reserve_tokens = tokens;
            }
            if let Some(tokens) = profile.keep_recent_tokens {
                settings.keep_recent_tokens = tokens;
            }
            if let Some(percent) = profile.threshold_percent {
                settings.threshold_percent = Some(percent);
            }
        }
        Ok(settings)
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

    /// `compaction.cachedSummary`, Bake's own: whether a compaction writes
    /// its summary from the conversation the provider has cached
    /// ([`crate::compaction::cached`]). Defaults to true; only `false`
    /// turns it off.
    pub fn cached_compaction(&self) -> bool {
        self.compaction
            .as_ref()
            .and_then(|compaction| compaction.get("cachedSummary"))
            .and_then(Value::as_bool)
            != Some(false)
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
        self.compaction_setting(field, model, token_count, "a non-negative safe integer")
            .map(|value| value.unwrap_or(default))
    }

    /// A `compaction` member resolved as Pi's `getCompactionTokenSetting`
    /// resolves it, read by `parse`: the model's override, then the
    /// ordinary setting. A value `parse` refuses is an error saying it
    /// expected `expected`.
    fn compaction_setting<T>(
        &self,
        field: &str,
        model: Option<(&str, &str)>,
        parse: impl Fn(&Value) -> Option<T>,
        expected: &str,
    ) -> Result<Option<T>, String> {
        let compaction = self.compaction.as_ref().and_then(Value::as_object);
        let ordinary = compaction.and_then(|compaction| compaction.get(field));
        let ordinary = match ordinary {
            None => None,
            Some(value) => Some(parse(value).ok_or_else(|| {
                format!(
                    "Invalid compaction.{field} setting: {}. Expected {expected}.",
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
                resolved = Some(parse(value).ok_or_else(|| {
                    format!(
                        "Invalid compaction.modelOverrides[\"{key}\"].{field} setting: {}. Expected {expected}.",
                        js_string(value)
                    )
                })?);
            }
        }
        Ok(resolved.or(ordinary))
    }

    /// Pi's `getCompactionSettings(model)`: each token setting resolved
    /// through the model's override (`compaction.modelOverrides`, keyed by
    /// the exact `provider/id`), the ordinary setting, then the default.
    /// Bake's `thresholdPercent`, a whole number from 1 to 100, resolves the
    /// same way and is unset by default.
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
            threshold_percent: self.compaction_setting(
                "thresholdPercent",
                model,
                |value| {
                    token_count(value)
                        .filter(|percent| (1..=100).contains(percent))
                        .and_then(|percent| u8::try_from(percent).ok())
                },
                "a whole number from 1 to 100",
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
        Self::load_file(&settings_path(home))
    }

    fn load_file(path: &Path) -> Self {
        match read_text_file(path, MAX_SETTINGS_FILE_BYTES) {
            Ok(None) => Self::default(),
            Ok(Some(text)) => Self::parse(&text, path),
            Err(error) => Self::failed(path, &error.to_string()),
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

/// One `modelProfiles` value: the choices that depend on the model. Each
/// member is unset unless the entry sets it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelProfile {
    /// `tools`: `+name` and `-name` entries applied after `defaultTools`.
    pub tools: Vec<String>,
    /// `codemode.mode`, over the `codemode.mode` setting.
    pub codemode_mode: Option<CodemodeMode>,
    /// `thinkingLevel`, over `modelThinkingLevels`.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// `compaction.reserveTokens`.
    pub reserve_tokens: Option<u64>,
    /// `compaction.keepRecentTokens`.
    pub keep_recent_tokens: Option<u64>,
    /// `compaction.thresholdPercent`.
    pub threshold_percent: Option<u8>,
    /// `prompt`: the text of the `model_notes` prompt section.
    pub prompt: Option<String>,
}

/// The name of the prompt section a profile's `prompt` becomes.
pub const MODEL_NOTES_SECTION: &str = "model_notes";

impl ModelProfile {
    /// Checks one entry; `key` names it in errors.
    fn parse(key: &str, value: &Value) -> Result<Self, String> {
        let invalid = |member: &str, value: &Value, expected: &str| {
            format!(
                "Invalid modelProfiles[\"{key}\"]{member} setting: {}. Expected {expected}.",
                js_string(value)
            )
        };
        let Some(entry) = value.as_object() else {
            return Err(invalid("", value, "an object"));
        };
        let mut profile = Self::default();
        for (member, value) in entry {
            match member.as_str() {
                "tools" => {
                    let expected = "a list of \"+name\" and \"-name\" entries";
                    let tools = value
                        .as_array()
                        .and_then(|tools| {
                            tools
                                .iter()
                                .map(|tool| tool.as_str().map(str::to_owned))
                                .collect::<Option<Vec<String>>>()
                        })
                        .filter(|tools| {
                            tools.iter().all(|tool| {
                                crate::tools::index::is_tool_modifier(tool)
                                    && tool.len() > 1
                                    && !tool.contains('*')
                            })
                        })
                        .ok_or_else(|| invalid(".tools", value, expected))?;
                    profile.tools = tools;
                }
                "codemode" => {
                    let mode = value
                        .as_object()
                        .filter(|codemode| codemode.keys().all(|key| key == "mode"))
                        .and_then(|codemode| codemode.get("mode"))
                        .and_then(Value::as_str)
                        .and_then(|mode| match mode {
                            "on" => Some(CodemodeMode::On),
                            "only" => Some(CodemodeMode::Only),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            invalid(
                                ".codemode",
                                value,
                                "{ \"mode\": \"on\" } or { \"mode\": \"only\" }",
                            )
                        })?;
                    profile.codemode_mode = Some(mode);
                }
                "thinkingLevel" => {
                    let level = value
                        .as_str()
                        .and_then(crate::model_registry::resolver::parse_thinking_level)
                        .ok_or_else(|| {
                            invalid(
                                ".thinkingLevel",
                                value,
                                "one of off, minimal, low, medium, high, xhigh, or max",
                            )
                        })?;
                    profile.thinking_level = Some(level);
                }
                "compaction" => {
                    let Some(compaction) = value.as_object() else {
                        return Err(invalid(".compaction", value, "an object"));
                    };
                    for (field, value) in compaction {
                        let member = format!(".compaction.{field}");
                        match field.as_str() {
                            "reserveTokens" | "keepRecentTokens" => {
                                let tokens = token_count(value).ok_or_else(|| {
                                    invalid(&member, value, "a non-negative safe integer")
                                })?;
                                if field == "reserveTokens" {
                                    profile.reserve_tokens = Some(tokens);
                                } else {
                                    profile.keep_recent_tokens = Some(tokens);
                                }
                            }
                            "thresholdPercent" => {
                                let percent = token_count(value)
                                    .filter(|percent| (1..=100).contains(percent))
                                    .and_then(|percent| u8::try_from(percent).ok())
                                    .ok_or_else(|| {
                                        invalid(&member, value, "a whole number from 1 to 100")
                                    })?;
                                profile.threshold_percent = Some(percent);
                            }
                            _ => {
                                return Err(format!(
                                    "Invalid modelProfiles[\"{key}\"].compaction setting: unknown member \"{field}\". Expected reserveTokens, keepRecentTokens, or thresholdPercent."
                                ));
                            }
                        }
                    }
                }
                "prompt" => {
                    let prompt = value
                        .as_str()
                        .ok_or_else(|| invalid(".prompt", value, "a string"))?;
                    profile.prompt = Some(prompt.to_owned());
                }
                _ => {
                    return Err(format!(
                        "Invalid modelProfiles[\"{key}\"] setting: unknown member \"{member}\". Expected tools, codemode, thinkingLevel, compaction, or prompt."
                    ));
                }
            }
        }
        Ok(profile)
    }

    /// `later` over this profile: each member it sets wins, and its `tools`
    /// follow these.
    fn overlay(&mut self, later: Self) {
        self.tools.extend(later.tools);
        self.codemode_mode = later.codemode_mode.or(self.codemode_mode);
        self.thinking_level = later.thinking_level.or(self.thinking_level);
        self.reserve_tokens = later.reserve_tokens.or(self.reserve_tokens);
        self.keep_recent_tokens = later.keep_recent_tokens.or(self.keep_recent_tokens);
        self.threshold_percent = later.threshold_percent.or(self.threshold_percent);
        self.prompt = later.prompt.or(self.prompt.take());
    }

    /// The first name in `tools` that is not one of `known`.
    pub fn unknown_tool<'a>(&'a self, known: &[String]) -> Option<&'a str> {
        self.tools
            .iter()
            .map(|entry| &entry[1..])
            .find(|name| !known.iter().any(|known| known == name))
    }

    /// `base`, the active tool names, with this profile's `tools` applied.
    pub fn active_tools(&self, base: &[String]) -> Vec<String> {
        crate::tools::index::apply_tool_modifiers(base, &self.tools)
    }
}

/// The `settings.json` path under a Bake home.
pub fn settings_path(home: &Path) -> PathBuf {
    home.join("settings.json")
}

/// Pi's string for a queue mode.
fn queue_mode_name(mode: QueueMode) -> &'static str {
    match mode {
        QueueMode::All => "all",
        QueueMode::OneAtATime => "one-at-a-time",
    }
}

/// Saves settings to `settings.json`, Pi's `SettingsManager` setters for the
/// global scope (see the module's deviations).
///
/// Each setter reads the file under the lock, changes only the members it
/// names, keeps every other member (including ones Bake does not read), and
/// returns the settings as now stored. A setter that changes nothing does not
/// write. The file is created owner-only when missing; a file that cannot be
/// read as a JSON object is left untouched and the setter fails. A setter
/// error for which [`StoreError::is_committed`] holds means the change was
/// written but may not survive a power loss; [`SettingsStore::load`] reads
/// it back. Setters block.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    file: JsonFileStore,
}

impl SettingsStore {
    /// The store of a Bake home's `settings.json`.
    pub fn new(home: &Path) -> Self {
        Self {
            file: JsonFileStore::new(settings_path(home), MAX_SETTINGS_FILE_BYTES),
        }
    }

    /// The file it writes.
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// The store with a failure injected into each update.
    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: crate::json_store::Fault) -> Self {
        self.file = self.file.with_fault(fault);
        self
    }

    /// Pi's `reload`: the settings now in the file, read as
    /// [`Settings::load`] reads them.
    pub fn load(&self) -> Settings {
        Settings::load_file(self.file.path())
    }

    fn update(
        &self,
        change: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<Settings, StoreError> {
        let updated = self.file.update(change)?;
        match updated.unsynced {
            Some(error) => Err(error),
            None => Ok(Settings::from_object(&updated.object)),
        }
    }

    /// Pi's `setDefaultProvider`.
    pub fn set_default_provider(&self, provider: &str) -> Result<Settings, StoreError> {
        self.update(|object| set_member(object, "defaultProvider", provider.into()))
    }

    /// Pi's `setDefaultModel`.
    pub fn set_default_model(&self, model: &str) -> Result<Settings, StoreError> {
        self.update(|object| set_member(object, "defaultModel", model.into()))
    }

    /// Pi's `setDefaultModelAndProvider`: both in one write.
    pub fn set_default_model_and_provider(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Settings, StoreError> {
        self.update(|object| {
            let provider = set_member(object, "defaultProvider", provider.into());
            set_member(object, "defaultModel", model.into()) | provider
        })
    }

    /// Pi's `setDefaultThinkingLevel`.
    pub fn set_default_thinking_level(
        &self,
        level: ModelThinkingLevel,
    ) -> Result<Settings, StoreError> {
        self.update(|object| set_member(object, "defaultThinkingLevel", level.as_str().into()))
    }

    /// Pi's `setModelThinkingLevel`: the level for `provider/id`, keeping the
    /// other models' entries. A `modelThinkingLevels` that is not an object
    /// is replaced.
    pub fn set_model_thinking_level(
        &self,
        provider: &str,
        id: &str,
        level: ModelThinkingLevel,
    ) -> Result<Settings, StoreError> {
        let key = format!("{provider}/{id}");
        self.update(|object| {
            let levels = object
                .entry("modelThinkingLevels")
                .or_insert_with(|| Value::Object(Map::new()));
            if !levels.is_object() {
                *levels = Value::Object(Map::new());
            }
            match levels.as_object_mut() {
                Some(levels) => set_member(levels, &key, level.as_str().into()),
                None => false,
            }
        })
    }

    /// Pi's `removeModelThinkingLevel`: drops `provider/id`, and the map
    /// when that leaves it empty.
    pub fn remove_model_thinking_level(
        &self,
        provider: &str,
        id: &str,
    ) -> Result<Settings, StoreError> {
        let key = format!("{provider}/{id}");
        self.update(|object| {
            let Some(levels) = object
                .get_mut("modelThinkingLevels")
                .and_then(Value::as_object_mut)
            else {
                return false;
            };
            let removed = remove_member(levels, &key);
            if levels.is_empty() {
                remove_member(object, "modelThinkingLevels");
            }
            removed
        })
    }

    /// Pi's `setSteeringMode`.
    pub fn set_steering_mode(&self, mode: QueueMode) -> Result<Settings, StoreError> {
        self.update(|object| set_member(object, "steeringMode", queue_mode_name(mode).into()))
    }

    /// Pi's `setFollowUpMode`.
    pub fn set_follow_up_mode(&self, mode: QueueMode) -> Result<Settings, StoreError> {
        self.update(|object| set_member(object, "followUpMode", queue_mode_name(mode).into()))
    }

    /// Bake's `permission.defaultPreset`, the preset new sessions take
    /// (`/permission`'s Ctrl+S), keeping the section's other members. A
    /// `permission` that is not an object is replaced.
    pub fn set_default_permission_preset(&self, preset: &str) -> Result<Settings, StoreError> {
        self.update(|object| {
            let section = object
                .entry("permission")
                .or_insert_with(|| Value::Object(Map::new()));
            if !section.is_object() {
                *section = Value::Object(Map::new());
            }
            match section.as_object_mut() {
                Some(section) => set_member(section, "defaultPreset", preset.into()),
                None => false,
            }
        })
    }

    /// Pi's `setCompactionEnabled`: `compaction.enabled`, keeping the
    /// section's other members. A `compaction` that is not an object is
    /// replaced.
    pub fn set_compaction_enabled(&self, enabled: bool) -> Result<Settings, StoreError> {
        self.set_compaction_member("enabled", Some(Value::Bool(enabled)))
    }

    /// Sets the `compaction` member `key`, or removes it when `value` is
    /// `None`, keeping the section's other members, as
    /// [`Self::set_compaction_enabled`] does. Bake's own, for the members
    /// `/settings` changes: `thresholdPercent`, `keepRecentTokens`, and
    /// `cachedSummary`.
    pub fn set_compaction_member(
        &self,
        key: &str,
        value: Option<Value>,
    ) -> Result<Settings, StoreError> {
        self.update(|object| {
            let Some(value) = value else {
                return object
                    .get_mut("compaction")
                    .and_then(Value::as_object_mut)
                    .is_some_and(|section| remove_member(section, key));
            };
            let section = object
                .entry("compaction")
                .or_insert_with(|| Value::Object(Map::new()));
            if !section.is_object() {
                *section = Value::Object(Map::new());
            }
            match section.as_object_mut() {
                Some(section) => set_member(section, key, value),
                None => false,
            }
        })
    }

    /// Sets `statusLine.<key>` to `on`, keeping the section's other members.
    /// A `statusLine` that is not an object is replaced. Bake's own.
    pub fn set_status_line(&self, key: &str, on: bool) -> Result<Settings, StoreError> {
        self.update(|object| {
            let section = object
                .entry("statusLine")
                .or_insert_with(|| Value::Object(Map::new()));
            if !section.is_object() {
                *section = Value::Object(Map::new());
            }
            match section.as_object_mut() {
                Some(section) => set_member(section, key, Value::Bool(on)),
                None => false,
            }
        })
    }

    /// `telecom`, Bake's own: whether a top-level session joins its
    /// repository's team when it starts.
    pub fn set_telecom(&self, on: bool) -> Result<Settings, StoreError> {
        self.update(|object| set_member(object, "telecom", on.into()))
    }

    /// `visualize`, Bake's own: whether an interactive session starts with
    /// the `visualize` tool.
    pub fn set_visualize(&self, on: bool) -> Result<Settings, StoreError> {
        self.update(|object| set_member(object, "visualize", on.into()))
    }

    /// Pi v1.5.2's `setMermaidRenderingMode`: `markdown.mermaid`, keeping
    /// the section's other members, such as `codeBlockIndent`. A `markdown`
    /// that is not an object is replaced.
    pub fn set_mermaid(&self, mode: MermaidRendering) -> Result<Settings, StoreError> {
        self.update(|object| {
            let section = object
                .entry("markdown")
                .or_insert_with(|| Value::Object(Map::new()));
            if !section.is_object() {
                *section = Value::Object(Map::new());
            }
            match section.as_object_mut() {
                Some(section) => set_member(section, "mermaid", mode.name().into()),
                None => false,
            }
        })
    }

    /// `branchSummary.skipPrompt`, keeping the section's other members. A
    /// `branchSummary` that is not an object is replaced. Bake's own.
    pub fn set_branch_summary_skip_prompt(&self, skip: bool) -> Result<Settings, StoreError> {
        self.update(|object| {
            let section = object
                .entry("branchSummary")
                .or_insert_with(|| Value::Object(Map::new()));
            if !section.is_object() {
                *section = Value::Object(Map::new());
            }
            match section.as_object_mut() {
                Some(section) => set_member(section, "skipPrompt", Value::Bool(skip)),
                None => false,
            }
        })
    }

    /// Changes the `cliproxyapi` member ([`CliProxySetting`]) under the
    /// file's lock: `change` sees the member as stored now (`None` when
    /// absent) and returns `None` to leave the file as it is, `Some(None)`
    /// to remove the member, or `Some(Some(value))` to set it. Returns the
    /// settings now stored and whether the member changed. After
    /// [`StoreError::is_committed`] the change is in the file. Bake's own
    /// member; Pi has no setter for it.
    pub fn update_cliproxyapi(
        &self,
        change: impl FnOnce(Option<&Value>) -> Option<Option<Value>>,
    ) -> Result<(Settings, bool), StoreError> {
        let mut written = false;
        let settings = self.update(|object| {
            written = match change(object.get(CLIPROXYAPI_MEMBER)) {
                None => false,
                Some(None) => remove_member(object, CLIPROXYAPI_MEMBER),
                Some(Some(next)) => set_member(object, CLIPROXYAPI_MEMBER, next),
            };
            written
        });
        match settings {
            Ok(settings) => Ok((settings, written)),
            Err(error) => Err(error),
        }
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
            Some(&["+grep", "-write"].map(String::from)[..])
        );
        let settings = Settings::parse(r#"{"defaultTools":"read"}"#, Path::new("s.json"));
        assert_eq!(settings.default_tools, Some(Vec::new()));
    }

    // `visualize` is on unless the file says `false`.
    #[test]
    fn visualize_is_on_unless_set_false() {
        let read = |text: &str| Settings::parse(text, Path::new("s.json")).visualize;
        assert!(read("{}"));
        assert!(read(r#"{"visualize":true}"#));
        assert!(read(r#"{"visualize":"no"}"#));
        assert!(!read(r#"{"visualize":false}"#));
    }

    // Pi v1.4.0 `settings-schema.ts` `showTerminalProgress`, and Bake's
    // own `terminal.notifications`; unset leaves the terminal to decide.
    #[test]
    fn reads_the_terminal_signal_settings() {
        let read = |text: &str| {
            let settings = Settings::parse(text, Path::new("s.json"));
            (
                settings.show_terminal_progress,
                settings.terminal_notifications,
            )
        };
        assert_eq!(
            read(r#"{"terminal":{"showTerminalProgress":true,"notifications":false}}"#),
            (Some(true), Some(false))
        );
        assert_eq!(
            read(r#"{"terminal":{"showTerminalProgress":"yes","notifications":1}}"#),
            (None, None)
        );
        assert_eq!(read(r#"{"terminal":true}"#), (None, None));
        assert_eq!(read("{}"), (None, None));
    }

    // A positive integer.
    #[test]
    fn reads_the_tool_argument_whitespace_bound() {
        let read =
            |text: &str| Settings::parse(text, Path::new("s.json")).max_tool_argument_whitespace;
        assert_eq!(read(r#"{"maxToolArgumentWhitespace":500}"#), Some(500));
        assert_eq!(read(r#"{"maxToolArgumentWhitespace":0}"#), None);
        assert_eq!(read(r#"{"maxToolArgumentWhitespace":-1}"#), None);
        assert_eq!(read(r#"{"maxToolArgumentWhitespace":1.5}"#), None);
        assert_eq!(read(r#"{"maxToolArgumentWhitespace":"9"}"#), None);
        assert_eq!(read("{}"), None);
    }

    // Bake's own: the `web_search` model on the CLIProxyAPI route.
    // Pi v1.3.0 `extensions/codemode/index.ts` `readMode` and
    // `readInlineBudget`, and `test/config-schemas.test.ts`'s budgets.
    #[test]
    fn reads_the_codemode_settings() {
        let read = |text: &str| {
            let settings = Settings::parse(text, Path::new("s.json"));
            (settings.codemode_mode, settings.codemode_inline_budget)
        };
        assert_eq!(read("{}"), (CodemodeMode::On, None));
        assert_eq!(
            read(r#"{"codemode":{"mode":"only","inlineBudget":0}}"#),
            (CodemodeMode::Only, Some(0.0))
        );
        assert_eq!(
            read(r#"{"codemode":{"mode":"on","inlineBudget":1500}}"#),
            (CodemodeMode::On, Some(1500.0))
        );
        assert_eq!(
            read(r#"{"codemode":{"mode":"ONLY","inlineBudget":-1}}"#),
            (CodemodeMode::On, None)
        );
        assert_eq!(
            read(r#"{"codemode":{"inlineBudget":"3000"}}"#),
            (CodemodeMode::On, None)
        );
        assert_eq!(read(r#"{"codemode":true}"#), (CodemodeMode::On, None));
    }

    #[test]
    fn reads_the_web_search_model() {
        let read = |text: &str| Settings::parse(text, Path::new("s.json")).web_search_model;
        assert_eq!(
            read(r#"{"webSearch":{"model":"claude-haiku-4-5"}}"#).as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(read(r#"{"webSearch":{"model":""}}"#), None);
        assert_eq!(read(r#"{"webSearch":{"model":7}}"#), None);
        assert_eq!(read(r#"{"webSearch":"x"}"#), None);
        assert_eq!(read("{}"), None);
    }

    #[test]
    fn reads_the_hooks_files() {
        let read = |text: &str| Settings::parse(text, Path::new("s.json")).hooks;
        assert_eq!(
            read(r#"{"hooks":{"claudeCode":"~/.claude/settings.json","codex":"codex.json"}}"#),
            HookSettings {
                claude_code: Some("~/.claude/settings.json".into()),
                codex: Some("codex.json".into()),
            }
        );
        assert_eq!(
            read(r#"{"hooks":{"claudeCode":"","codex":7}}"#),
            HookSettings::default()
        );
        assert_eq!(read(r#"{"hooks":"x"}"#), HookSettings::default());
        assert!(!read("{}").is_set());
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
                    keep_recent_tokens: 10000,
                    threshold_percent: None,
                })
            );
            assert_eq!(
                manager.compaction_settings(None),
                Ok(CompactionSettings {
                    enabled: true,
                    reserve_tokens: 8192,
                    keep_recent_tokens: 10000,
                    threshold_percent: None,
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

        // Bake: `thresholdPercent` is a whole number from 1 to 100,
        // resolved through the model's override as the token settings are,
        // and an invalid one is an error naming it.
        #[test]
        fn threshold_percent_resolves_and_is_checked() {
            let threshold = |value: Value, model| {
                settings(value)
                    .compaction_settings(model)
                    .map(|compaction| compaction.threshold_percent)
            };
            assert_eq!(threshold(json!({}), None), Ok(None));
            let both = json!({ "compaction": {
                "thresholdPercent": 80,
                "modelOverrides": { KEY: { "thresholdPercent": 60 } },
            }});
            assert_eq!(threshold(both.clone(), None), Ok(Some(80)));
            assert_eq!(threshold(both, Some(MODEL)), Ok(Some(60)));
            assert_eq!(
                threshold(json!({ "compaction": { "thresholdPercent": 100 } }), None),
                Ok(Some(100))
            );
            for invalid in [json!(0), json!(101), json!(50.5), json!("80"), json!(-1)] {
                let message = format!(
                    "Invalid compaction.thresholdPercent setting: {}. Expected a whole number from 1 to 100.",
                    js_string(&invalid)
                );
                assert_eq!(
                    threshold(
                        json!({ "compaction": { "thresholdPercent": invalid } }),
                        None
                    ),
                    Err(message)
                );
            }
            assert_eq!(
                threshold(
                    json!({ "compaction": { "modelOverrides": { KEY: { "thresholdPercent": 0 } } } }),
                    Some(MODEL)
                ),
                Err(format!(
                    "Invalid compaction.modelOverrides[\"{KEY}\"].thresholdPercent setting: 0. Expected a whole number from 1 to 100."
                ))
            );
            // On unless turned off; a value that is not a boolean is ignored.
            assert!(settings(json!({})).cached_compaction());
            assert!(
                settings(json!({ "compaction": { "cachedSummary": "no" } })).cached_compaction()
            );
            assert!(
                !settings(json!({ "compaction": { "cachedSummary": false } })).cached_compaction()
            );
        }

        // Pi: "accepts zero in ordinary settings and model overrides".
        #[test]
        fn zero_is_accepted() {
            let zero = CompactionSettings {
                enabled: true,
                reserve_tokens: 0,
                keep_recent_tokens: 0,
                threshold_percent: None,
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
            assert!(!manager.branch_summary_skip_prompt);
            let manager = settings(json!({
                "retry": { "enabled": false, "maxRetries": 5, "baseDelayMs": 1, "maxAgentDelayMs": 5 },
                "branchSummary": { "reserveTokens": 100, "skipPrompt": true },
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
            assert!(manager.branch_summary_skip_prompt);
        }
    }

    // Ports of Pi v1.2.0 `test/settings-manager.test.ts` ("preserves
    // externally added settings", "reload") and
    // `test/settings-manager-bug.test.ts` for the setters Bake has. Not
    // ported: project settings and trust, and the settings Bake does not
    // read (theme, packages, and the TUI settings).
    mod store {
        use super::*;
        use crate::test_support::TempDir;
        use serde_json::json;

        fn read(path: &Path) -> Value {
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
        }

        // A directory flush that fails after the replacement: the setter
        // reports a committed error and the new value reads back.
        #[test]
        fn an_unsynced_write_is_reported_and_reads_back() {
            use crate::json_store::{Fault, JsonFileStore};
            let dir = TempDir::new("settings-unsynced");
            let store = SettingsStore {
                file: JsonFileStore::new(settings_path(dir.path()), MAX_SETTINGS_FILE_BYTES)
                    .with_fault(Fault::DirectorySync),
            };
            let error = store.set_default_provider("cliproxyapi").unwrap_err();
            assert!(error.is_committed(), "{error:?}");
            assert_eq!(
                store.load().default_provider.as_deref(),
                Some("cliproxyapi")
            );
            assert_eq!(
                read(store.path()),
                json!({ "defaultProvider": "cliproxyapi" })
            );
        }

        // Pi: "should preserve enabledModels when changing thinking level"
        // and "should preserve file changes to packages array when changing
        // unrelated setting".
        #[test]
        fn a_setter_keeps_external_and_unknown_settings() {
            let dir = TempDir::new("settings-store");
            let store = SettingsStore::new(dir.path());
            assert_eq!(store.path(), dir.join("settings.json"));
            std::fs::write(
                store.path(),
                r#"{"theme":"dark","defaultModel":"claude-sonnet"}"#,
            )
            .unwrap();
            // Another process edits the file after this one started.
            std::fs::write(
                store.path(),
                r#"{"theme":"dark","defaultModel":"claude-sonnet","enabledModels":["claude-opus-4-5","gpt-5.2-codex"],"packages":["npm:pi-mcp-adapter"],"queueMode":"all"}"#,
            )
            .unwrap();
            let settings = store
                .set_default_thinking_level(ModelThinkingLevel::High)
                .unwrap();
            assert_eq!(
                settings.default_thinking_level,
                Some(ModelThinkingLevel::High)
            );
            assert_eq!(settings.steering_mode, QueueMode::All);
            assert_eq!(
                read(store.path()),
                json!({
                    "theme": "dark", "defaultModel": "claude-sonnet",
                    "enabledModels": ["claude-opus-4-5", "gpt-5.2-codex"],
                    "packages": ["npm:pi-mcp-adapter"], "queueMode": "all",
                    "defaultThinkingLevel": "high",
                })
            );
        }

        // Pi v1.5.2's `getMermaidRenderingMode` and
        // `setMermaidRenderingMode`.
        #[test]
        fn markdown_mermaid_reads_as_pi_reads_it_and_keeps_its_section() {
            let dir = TempDir::new("settings-mermaid");
            let store = SettingsStore::new(dir.path());
            assert_eq!(
                Settings::load(dir.path()).mermaid,
                MermaidRendering::Streaming
            );
            for (value, mode) in [
                (json!("off"), MermaidRendering::Off),
                (json!("final"), MermaidRendering::Final),
                (json!("streaming"), MermaidRendering::Streaming),
                (json!("Final"), MermaidRendering::Streaming),
                (json!(true), MermaidRendering::Streaming),
            ] {
                let text = json!({ "markdown": { "mermaid": value } }).to_string();
                std::fs::write(store.path(), text).unwrap();
                assert_eq!(Settings::load(dir.path()).mermaid, mode, "{value}");
            }
            std::fs::write(
                store.path(),
                r#"{"markdown":{"codeBlockIndent":"    ","mermaid":"streaming"},"x":1}"#,
            )
            .unwrap();
            let settings = store.set_mermaid(MermaidRendering::Final).unwrap();
            assert_eq!(settings.mermaid, MermaidRendering::Final);
            assert_eq!(
                read(store.path()),
                json!({ "markdown": { "codeBlockIndent": "    ", "mermaid": "final" }, "x": 1 })
            );
            std::fs::write(store.path(), r#"{"markdown":"broken"}"#).unwrap();
            store.set_mermaid(MermaidRendering::Off).unwrap();
            assert_eq!(
                read(store.path()),
                json!({ "markdown": { "mermaid": "off" } })
            );
        }

        // Pi: "should let in-memory changes override file changes for same
        // key", and persistence across a restart.
        #[test]
        fn setters_persist_and_the_last_write_of_a_key_wins() {
            let dir = TempDir::new("settings-persist");
            let store = SettingsStore::new(dir.path());
            store
                .set_default_model_and_provider("cliproxyapi", "gpt-5")
                .unwrap();
            std::fs::write(
                store.path(),
                r#"{"defaultProvider":"external","defaultModel":"external-model","x":1}"#,
            )
            .unwrap();
            store.set_default_provider("cliproxyapi").unwrap();
            store.set_steering_mode(QueueMode::All).unwrap();
            store.set_follow_up_mode(QueueMode::All).unwrap();
            store
                .set_model_thinking_level("p", "m", ModelThinkingLevel::Low)
                .unwrap();
            let settings = store.set_default_model("gpt-5").unwrap();
            let loaded = Settings::load(dir.path());
            assert_eq!(settings, loaded);
            assert_eq!(loaded.default_provider.as_deref(), Some("cliproxyapi"));
            assert_eq!(loaded.default_model.as_deref(), Some("gpt-5"));
            assert_eq!(loaded.steering_mode, QueueMode::All);
            assert_eq!(loaded.follow_up_mode, QueueMode::All);
            assert_eq!(
                loaded.model_thinking_level("p", "m"),
                Some(ModelThinkingLevel::Low)
            );
            assert_eq!(read(store.path())["x"], 1);
            assert_eq!(store.load(), loaded);
        }

        // Bake: `/settings` sets and removes compaction members, keeping
        // the section's others, and saves `branchSummary.skipPrompt`.
        #[test]
        fn compaction_members_are_set_and_removed_in_place() {
            let dir = TempDir::new("settings-compaction");
            let store = SettingsStore::new(dir.path());
            std::fs::write(
                store.path(),
                r#"{"compaction":{"reserveTokens":9000},"branchSummary":"broken"}"#,
            )
            .unwrap();
            store
                .set_compaction_member("thresholdPercent", Some(json!(80)))
                .unwrap();
            store
                .set_compaction_member("cachedSummary", Some(json!(true)))
                .unwrap();
            let settings = store.set_branch_summary_skip_prompt(true).unwrap();
            assert!(settings.cached_compaction());
            assert!(settings.branch_summary_skip_prompt);
            assert!(!settings.status_line.any());
            store.set_status_line("cache", true).unwrap();
            let settings = store.set_status_line("tokens", false).unwrap();
            assert_eq!(
                settings.status_line,
                StatusLineSettings {
                    tokens: false,
                    cache: true
                }
            );
            assert_eq!(
                read(store.path())["statusLine"],
                json!({ "cache": true, "tokens": false })
            );
            std::fs::write(
                store.path(),
                serde_json::to_vec(&json!({
                    "compaction": { "reserveTokens": 9000, "thresholdPercent": 80, "cachedSummary": true },
                    "branchSummary": { "skipPrompt": true },
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                settings
                    .compaction_settings(None)
                    .map(|compaction| compaction.threshold_percent),
                Ok(Some(80))
            );
            assert_eq!(
                read(store.path()),
                json!({
                    "compaction": { "reserveTokens": 9000, "thresholdPercent": 80, "cachedSummary": true },
                    "branchSummary": { "skipPrompt": true },
                })
            );
            let settings = store
                .set_compaction_member("thresholdPercent", None)
                .unwrap();
            assert_eq!(
                settings
                    .compaction_settings(None)
                    .map(|compaction| compaction.threshold_percent),
                Ok(None)
            );
            assert_eq!(
                read(store.path())["compaction"],
                json!({ "reserveTokens": 9000, "cachedSummary": true })
            );
            // Removing what is not there writes nothing.
            let before = std::fs::read(store.path()).unwrap();
            store
                .set_compaction_member("keepRecentTokens", None)
                .unwrap();
            assert_eq!(std::fs::read(store.path()).unwrap(), before);
            // The in-memory form follows the same rule.
            let mut memory = Settings::default();
            set_compaction_member_in(&mut memory, "keepRecentTokens", Some(json!(30000)));
            set_compaction_enabled_in(&mut memory, false);
            assert_eq!(
                memory.compaction,
                Some(json!({ "keepRecentTokens": 30000, "enabled": false }))
            );
            set_compaction_member_in(&mut memory, "keepRecentTokens", None);
            assert_eq!(memory.compaction, Some(json!({ "enabled": false })));
        }

        // Pi's `removeModelThinkingLevel`, keyed per model here.
        #[test]
        fn model_thinking_levels_change_one_entry() {
            let dir = TempDir::new("settings-levels");
            let store = SettingsStore::new(dir.path());
            std::fs::write(store.path(), r#"{"modelThinkingLevels":"broken"}"#).unwrap();
            store
                .set_model_thinking_level("a", "x/y", ModelThinkingLevel::Max)
                .unwrap();
            std::fs::write(
                store.path(),
                r#"{"modelThinkingLevels":{"a/x/y":"max","other/m":"low"}}"#,
            )
            .unwrap();
            let settings = store.remove_model_thinking_level("a", "x/y").unwrap();
            assert_eq!(settings.model_thinking_level("a", "x/y"), None);
            assert_eq!(
                read(store.path()),
                json!({ "modelThinkingLevels": { "other/m": "low" } })
            );
            store.remove_model_thinking_level("other", "m").unwrap();
            assert_eq!(read(store.path()), json!({}));
        }

        // Pi: "should keep previous settings and report the file path when
        // the file is invalid", with the write refused and the bytes kept.
        #[test]
        fn a_broken_file_is_not_overwritten() {
            let dir = TempDir::new("settings-broken");
            let store = SettingsStore::new(dir.path());
            for broken in ["{\"theme\":", "[]", "null"] {
                std::fs::write(store.path(), broken).unwrap();
                let error = store.set_default_model("m").unwrap_err();
                assert!(matches!(error, StoreError::Refused { .. }), "{error:?}");
                assert_eq!(std::fs::read_to_string(store.path()).unwrap(), broken);
                assert_eq!(store.load().warnings.len(), 1);
            }
        }

        #[test]
        fn an_unchanged_value_does_not_create_the_file() {
            let dir = TempDir::new("settings-noop");
            let store = SettingsStore::new(dir.path());
            store.remove_model_thinking_level("p", "m").unwrap();
            assert!(!store.path().exists());
            store.set_default_model("m").unwrap();
            let modified = std::fs::metadata(store.path()).unwrap().modified().unwrap();
            let text = std::fs::read_to_string(store.path()).unwrap();
            store.set_default_model("m").unwrap();
            assert_eq!(std::fs::read_to_string(store.path()).unwrap(), text);
            assert_eq!(
                std::fs::metadata(store.path()).unwrap().modified().unwrap(),
                modified
            );
        }
    }

    fn profiled(value: serde_json::Value) -> Settings {
        Settings::from_object(value.as_object().expect("object"))
    }

    fn names(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| (*entry).to_owned()).collect()
    }

    // Entries apply in the order written: each later match overrides the
    // members it sets, `tools` lists join, and patterns match as
    // `enabledModels` patterns do (`provider/id`, a bare id, a glob).
    #[test]
    fn model_profiles_apply_in_order_with_later_members_winning() {
        let settings = profiled(serde_json::json!({
            "modelProfiles": {
                "faux/*": {
                    "tools": ["+grep"],
                    "thinkingLevel": "high",
                    "codemode": { "mode": "only" },
                    "prompt": "first",
                    "compaction": { "reserveTokens": 10, "keepRecentTokens": 20 }
                },
                "m-1": { "tools": ["-grep", "+ls"], "thinkingLevel": "low", "compaction": { "keepRecentTokens": 30 } },
                "other/m-1": { "prompt": "never" }
            }
        }));
        let one = bake_ai::providers::faux::faux_model("m-1");
        let profile = settings.model_profile(&one);
        assert_eq!(profile.tools, names(&["+grep", "-grep", "+ls"]));
        assert_eq!(profile.thinking_level, Some(ModelThinkingLevel::Low));
        assert_eq!(profile.codemode_mode, Some(CodemodeMode::Only));
        assert_eq!(profile.prompt.as_deref(), Some("first"));
        assert_eq!(profile.reserve_tokens, Some(10));
        assert_eq!(profile.keep_recent_tokens, Some(30));
        assert_eq!(profile.threshold_percent, None);
        assert_eq!(
            profile.active_tools(&names(&["read", "grep"])),
            names(&["read", "ls"])
        );
        let two = settings.model_profile(&bake_ai::providers::faux::faux_model("m-2"));
        assert_eq!(two.tools, names(&["+grep"]));
        assert_eq!(two.thinking_level, Some(ModelThinkingLevel::High));
        let mut elsewhere = bake_ai::providers::faux::faux_model("m-1");
        elsewhere.provider = "other".into();
        assert_eq!(
            settings.model_profile(&elsewhere).prompt.as_deref(),
            Some("never")
        );
        assert!(settings.model_profiles().is_ok());
    }

    // The profile's level wins over `modelThinkingLevels`, and its
    // compaction members over `compaction.modelOverrides`; an explicit tool
    // selection drops only its `tools`.
    #[test]
    fn a_profile_wins_over_the_per_model_settings() {
        let mut settings = profiled(serde_json::json!({
            "modelThinkingLevels": { "faux/m": "max", "faux/n": "medium" },
            "compaction": { "modelOverrides": { "faux/m": { "reserveTokens": 5, "thresholdPercent": 80 } } },
            "modelProfiles": {
                "faux/m": { "tools": ["+ls"], "thinkingLevel": "minimal", "compaction": { "reserveTokens": 7 } }
            }
        }));
        let m = bake_ai::providers::faux::faux_model("m");
        let n = bake_ai::providers::faux::faux_model("n");
        assert_eq!(
            settings.thinking_level_for(&m),
            Some(ModelThinkingLevel::Minimal)
        );
        assert_eq!(
            settings.thinking_level_for(&n),
            Some(ModelThinkingLevel::Medium)
        );
        let compaction = settings
            .compaction_settings_for(Some(&m))
            .expect("settings");
        assert_eq!(compaction.reserve_tokens, 7);
        assert_eq!(compaction.threshold_percent, Some(80));
        settings.explicit_tools = true;
        let profile = settings.model_profile(&m);
        assert!(profile.tools.is_empty());
        assert_eq!(profile.thinking_level, Some(ModelThinkingLevel::Minimal));
    }

    // Each malformed member is refused with the entry and member it is in.
    #[test]
    fn malformed_model_profiles_are_refused_with_their_place() {
        let cases = [
            (
                serde_json::json!([]),
                "Invalid modelProfiles setting: . Expected an object of model patterns.",
            ),
            (
                serde_json::json!({ "m": 1 }),
                "Invalid modelProfiles[\"m\"] setting: 1. Expected an object.",
            ),
            (
                serde_json::json!({ "m": { "tools": ["grep"] } }),
                "Invalid modelProfiles[\"m\"].tools setting: grep. Expected a list of \"+name\" and \"-name\" entries.",
            ),
            (
                serde_json::json!({ "m": { "tools": ["+gr*"] } }),
                "Invalid modelProfiles[\"m\"].tools setting: +gr*. Expected a list of \"+name\" and \"-name\" entries.",
            ),
            (
                serde_json::json!({ "m": { "tools": ["+"] } }),
                "Invalid modelProfiles[\"m\"].tools setting: +. Expected a list of \"+name\" and \"-name\" entries.",
            ),
            (
                serde_json::json!({ "m": { "codemode": { "mode": "off" } } }),
                "Invalid modelProfiles[\"m\"].codemode setting: [object Object]. Expected { \"mode\": \"on\" } or { \"mode\": \"only\" }.",
            ),
            (
                serde_json::json!({ "m": { "compaction": { "thresholdPercent": 0 } } }),
                "Invalid modelProfiles[\"m\"].compaction.thresholdPercent setting: 0. Expected a whole number from 1 to 100.",
            ),
            (
                serde_json::json!({ "m": { "compaction": { "reserveTokens": -1 } } }),
                "Invalid modelProfiles[\"m\"].compaction.reserveTokens setting: -1. Expected a non-negative safe integer.",
            ),
            (
                serde_json::json!({ "m": { "compaction": { "enabled": false } } }),
                "Invalid modelProfiles[\"m\"].compaction setting: unknown member \"enabled\". Expected reserveTokens, keepRecentTokens, or thresholdPercent.",
            ),
            (
                serde_json::json!({ "m": { "prompt": 3 } }),
                "Invalid modelProfiles[\"m\"].prompt setting: 3. Expected a string.",
            ),
            (
                serde_json::json!({ "m": { "temperature": 1 } }),
                "Invalid modelProfiles[\"m\"] setting: unknown member \"temperature\". Expected tools, codemode, thinkingLevel, compaction, or prompt.",
            ),
        ];
        for (value, expected) in cases {
            let settings = profiled(serde_json::json!({ "modelProfiles": value }));
            assert_eq!(
                settings.model_profiles(),
                Err(expected.to_owned()),
                "{value}"
            );
            // A refused setting resolves to no profile rather than half of one.
            assert_eq!(
                settings.model_profile(&bake_ai::providers::faux::faux_model("m")),
                ModelProfile::default()
            );
        }
        let unknown =
            profiled(serde_json::json!({ "modelProfiles": { "m": { "tools": ["+nope"] } } }))
                .model_profiles()
                .expect("valid shape");
        assert_eq!(unknown[0].1.unknown_tool(&names(&["read"])), Some("nope"));
    }

    // `nestedContextFiles`: a boolean, false when unset; anything else
    // stops the session's start.
    #[test]
    fn nested_context_files_is_a_boolean() {
        let read = |value: serde_json::Value| {
            profiled(serde_json::json!({ "nestedContextFiles": value })).nested_context_files()
        };
        assert_eq!(Settings::default().nested_context_files(), Ok(false));
        assert_eq!(read(serde_json::json!(true)), Ok(true));
        assert_eq!(read(serde_json::json!(false)), Ok(false));
        for invalid in [
            serde_json::json!("true"),
            serde_json::json!(1),
            serde_json::json!(null),
            serde_json::json!({}),
        ] {
            assert_eq!(
                read(invalid.clone()),
                Err("Invalid nestedContextFiles setting: expected true or false.".into()),
                "{invalid}"
            );
        }
    }

    // `skills.listingTokens`: a positive integer, 4000 when unset; anything
    // else stops the session's start.
    #[test]
    fn skills_listing_tokens_is_a_positive_integer() {
        let tokens = |value: serde_json::Value| {
            profiled(serde_json::json!({ "skills": { "listingTokens": value } }))
                .skills_listing_tokens()
        };
        assert_eq!(Settings::default().skills_listing_tokens(), Ok(4000));
        assert_eq!(
            profiled(serde_json::json!({ "skills": {} })).skills_listing_tokens(),
            Ok(4000)
        );
        for valid in [1_u64, 400, 100_000] {
            assert_eq!(tokens(serde_json::json!(valid)), Ok(valid));
        }
        for invalid in [
            serde_json::json!(0),
            serde_json::json!(-5),
            serde_json::json!(1.5),
            serde_json::json!("1500"),
            serde_json::json!(null),
            serde_json::json!(true),
        ] {
            assert_eq!(
                tokens(invalid.clone()),
                Err("Invalid skills.listingTokens setting: expected a positive integer.".into()),
                "{invalid}"
            );
        }
    }

    proptest::proptest! {
        // Any JSON as `modelProfiles` is either refused or yields profiles
        // whose tools are all exact modifiers; resolving never panics.
        #[test]
        fn model_profiles_parse_any_json_safely(text in "[\\[\\]{}\":,a-z0-9+*/ .-]{0,64}") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                let settings = profiled(serde_json::json!({ "modelProfiles": value }));
                if let Ok(profiles) = settings.model_profiles() {
                    for (_, profile) in profiles {
                        let exact = profile.tools.iter().all(|tool| {
                            crate::tools::index::is_tool_modifier(tool) && tool.len() > 1 && !tool.contains('*')
                        });
                        proptest::prop_assert!(exact);
                    }
                }
                let _ = settings.model_profile(&bake_ai::providers::faux::faux_model("m"));
            }
        }
    }
}
