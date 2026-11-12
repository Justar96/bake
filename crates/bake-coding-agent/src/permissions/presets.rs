//! Permission presets, ported from Bake's TypeScript
//! `packages/interaction/permission-presets/src/{index,types}.ts`: named
//! bundles of the two knobs, the sandbox mode and the approval policy, the
//! derived `custom` value, and the default preset for new sessions.
//!
//! The table comes from `$BAKE_HOME/settings.json` `permission.presets`
//! (Pi's settings shape) and defaults to the TypeScript table, with
//! friendlier labels. The TypeScript Auto review integration
//! (`registerAuto`) is not ported; its name stays reserved.
//!
//! The composition's defaults are Pi's: full access with no approval
//! prompts, since Pi v1.5.2 runs every tool unconfined and "does not ask
//! for approval before every tool call" (its `docs/security.md`). The
//! TypeScript composition's defaults, `workspace-write` and `ask`, are
//! [`KnobDefaults::TYPESCRIPT`].

use bake_sandbox::SandboxMode;
use serde_json::{Map, Value};

use super::approval::ApprovalPolicy;

/// The derived value when the knobs match no preset (`CUSTOM_PRESET`); never
/// a switch target or a stored selection.
pub const CUSTOM_PRESET: &str = "custom";
/// The name reserved for the per-call review integration (`AUTO_PRESET`).
pub const AUTO_PRESET: &str = "auto";
/// The settings namespace (`PERMISSION_SETTINGS_NAMESPACE`), in
/// `settings.json` and in the 0.3 `settings.yaml`.
pub const PERMISSION_SETTINGS_NAMESPACE: &str = "permission";

/// One preset's knob bundle and presentation (`PresetSpec`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetSpec {
    /// The sandbox mode the preset selects.
    pub sandbox: SandboxMode,
    /// The approval policy the preset selects.
    pub approval: ApprovalPolicy,
    /// The label a client shows; the table key when absent.
    pub name: Option<String>,
    /// One sentence on what the preset means.
    pub description: Option<String>,
}

/// A preset as a client presents it (`PresetOption`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetOption {
    /// The preset key, or `custom`.
    pub value: String,
    /// The label.
    pub name: String,
    /// The description, when configured.
    pub description: Option<String>,
}

/// The composition's knob defaults, which apply where a session has no
/// value of its own: Pi's full access, `danger-full-access` and `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnobDefaults {
    /// The default sandbox mode (`ctx.shell.sandboxMode`).
    pub sandbox: SandboxMode,
    /// The default approval policy (`ctx.approval.config.policy`).
    pub approval: ApprovalPolicy,
}

impl KnobDefaults {
    /// Pi's: every tool unconfined, no approval prompts.
    pub const PI: Self = Self {
        sandbox: SandboxMode::DangerFullAccess,
        approval: ApprovalPolicy::Never,
    };

    /// Every shipped 0.3 profile's (`packages/bundle/base/cordis.patch.yml`),
    /// the default through Bake 0.4.5: `workspace-write` and `ask`. A table
    /// without a full-access preset falls back to the preset matching these
    /// ([`PresetTable::default_preset`]).
    pub const TYPESCRIPT: Self = Self {
        sandbox: SandboxMode::WorkspaceWrite,
        approval: ApprovalPolicy::Ask,
    };
}

impl Default for KnobDefaults {
    fn default() -> Self {
        Self::PI
    }
}

/// A session's own knob values (`KnobState`): the last stored value of each
/// knob, `None` before one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnobState {
    /// The last selected preset.
    pub preset: Option<String>,
    /// The last sandbox mode.
    pub sandbox: Option<SandboxMode>,
    /// The last approval policy.
    pub approval: Option<ApprovalPolicy>,
}

impl KnobState {
    /// The values in force: the session's own, else `defaults`.
    pub fn effective(&self, defaults: KnobDefaults) -> (SandboxMode, ApprovalPolicy) {
        (
            self.sandbox.unwrap_or(defaults.sandbox),
            self.approval.unwrap_or(defaults.approval),
        )
    }
}

/// The preset table, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetTable {
    presets: Vec<(String, PresetSpec)>,
}

impl Default for PresetTable {
    /// The TypeScript default table (`PermissionPresetService.Config`): its
    /// keys, order, and descriptions, with labels that say what each
    /// preset does rather than repeat its key.
    fn default() -> Self {
        Self {
            presets: vec![
                (
                    "workspace-write".to_owned(),
                    PresetSpec {
                        sandbox: SandboxMode::WorkspaceWrite,
                        approval: ApprovalPolicy::Ask,
                        name: Some("Workspace, ask for more".to_owned()),
                        description: Some("Write inside the workspace and permitted temporary directories; wider retries require approval.".to_owned()),
                    },
                ),
                (
                    "danger-full-access".to_owned(),
                    PresetSpec {
                        sandbox: SandboxMode::DangerFullAccess,
                        approval: ApprovalPolicy::Never,
                        name: Some("Full access".to_owned()),
                        description: Some("Full file access without approval prompts.".to_owned()),
                    },
                ),
            ],
        }
    }
}

impl PresetTable {
    /// A table of `presets`, refusing the reserved names as the TypeScript
    /// constructor does.
    pub fn new(presets: Vec<(String, PresetSpec)>) -> Result<Self, String> {
        if presets.iter().any(|(name, _)| name == CUSTOM_PRESET) {
            return Err(format!(
                "permission: \"{CUSTOM_PRESET}\" is reserved for the derived not-a-preset state and cannot name a table entry"
            ));
        }
        if presets.iter().any(|(name, _)| name == AUTO_PRESET) {
            return Err(format!(
                "permission: \"{AUTO_PRESET}\" is reserved and cannot name a configured preset"
            ));
        }
        Ok(Self { presets })
    }

    /// The table `settings.json` configures under `permission.presets`, or
    /// the default table when it configures none. A malformed table is an
    /// error, never silently replaced, since its presets bound what tools
    /// may do.
    pub fn from_settings(permission: Option<&Value>) -> Result<Self, String> {
        let Some(section) = settings_section(permission)? else {
            return Ok(Self::default());
        };
        let table = match section.get("presets") {
            None | Some(Value::Null) => return Ok(Self::default()),
            Some(Value::Object(table)) => table,
            Some(_) => {
                return Err(
                    "permission: settings.json permission.presets must be an object".into(),
                );
            }
        };
        let mut presets = Vec::with_capacity(table.len());
        for (key, entry) in table {
            presets.push((key.clone(), preset_spec(key, entry)?));
        }
        Self::new(presets)
    }

    /// The preset names, in declaration order (`names`).
    pub fn names(&self) -> Vec<&str> {
        self.presets.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// A configured preset.
    pub fn spec(&self, name: &str) -> Option<&PresetSpec> {
        self.presets
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, spec)| spec)
    }

    /// A configured preset, or the TypeScript `resolve` error.
    pub fn resolve(&self, name: &str) -> Result<&PresetSpec, String> {
        self.spec(name).ok_or_else(|| {
            format!(
                "permission: unknown preset \"{name}\" (known: {})",
                self.names().join(", ")
            )
        })
    }

    /// How a client presents `name` (`optionOf`).
    pub fn option_of(&self, name: &str) -> Result<PresetOption, String> {
        if name == CUSTOM_PRESET {
            return Ok(PresetOption {
                value: CUSTOM_PRESET.to_owned(),
                name: "Custom".to_owned(),
                description: Some(
                    "Current sandbox and approval settings do not match a preset.".to_owned(),
                ),
            });
        }
        let spec = self.resolve(name)?;
        Ok(PresetOption {
            value: name.to_owned(),
            name: spec.name.clone().unwrap_or_else(|| name.to_owned()),
            description: spec.description.clone(),
        })
    }

    /// The preset matching the effective knobs (`derive`): a still-matching
    /// last selection wins ties between presets with the same bundle, then
    /// the first match in table order, else [`CUSTOM_PRESET`].
    pub fn derive(&self, state: &KnobState, defaults: KnobDefaults) -> String {
        let (sandbox, approval) = state.effective(defaults);
        let matches = |spec: &PresetSpec| spec.sandbox == sandbox && spec.approval == approval;
        if let Some(preset) = &state.preset
            && self.spec(preset).is_some_and(matches)
        {
            return preset.clone();
        }
        self.presets
            .iter()
            .find(|(_, spec)| matches(spec))
            .map_or_else(|| CUSTOM_PRESET.to_owned(), |(name, _)| name.clone())
    }

    /// The default preset for new sessions: `configured` when given, else
    /// the first preset matching `defaults`, the composition's knob
    /// defaults (Pi's full access); for a table without one, the first
    /// preset matching [`KnobDefaults::TYPESCRIPT`], the default through
    /// Bake 0.4.5; else the table's first preset. `configured` resolves as
    /// the TypeScript constructor resolves `defaultPreset`; only an empty
    /// table has no default.
    pub fn default_preset(
        &self,
        configured: Option<&str>,
        defaults: KnobDefaults,
    ) -> Result<String, String> {
        if let Some(name) = configured {
            self.resolve(name)?;
            return Ok(name.to_owned());
        }
        [defaults, KnobDefaults::TYPESCRIPT]
            .into_iter()
            .map(|defaults| self.derive(&KnobState::default(), defaults))
            .find(|name| name != CUSTOM_PRESET)
            .or_else(|| self.presets.first().map(|(name, _)| name.clone()))
            .ok_or_else(|| {
                "permission: settings.json permission.presets names no preset; configure at least one"
                    .into()
            })
    }

    /// The first preset pairing `danger-full-access` with `never`: the one
    /// an approval's full-access answer switches the session to, as
    /// `/permission` would. `None` when the table has none, so no answer
    /// offers it.
    pub fn full_access_preset(&self) -> Option<&str> {
        self.presets
            .iter()
            .find(|(_, spec)| {
                spec.sandbox == SandboxMode::DangerFullAccess
                    && spec.approval == ApprovalPolicy::Never
            })
            .map(|(name, _)| name.as_str())
    }

    /// Every preset, in declaration order, with its spec.
    pub fn presets(&self) -> impl Iterator<Item = (&str, &PresetSpec)> {
        self.presets
            .iter()
            .map(|(name, spec)| (name.as_str(), spec))
    }
}

/// `settings.json` `permission.defaultPreset`, when set.
pub fn settings_default_preset(permission: Option<&Value>) -> Result<Option<String>, String> {
    let Some(section) = settings_section(permission)? else {
        return Ok(None);
    };
    match section.get("defaultPreset") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) => Ok(Some(name.clone())),
        Some(_) => {
            Err("permission: settings.json permission.defaultPreset must be a string".into())
        }
    }
}

fn settings_section(permission: Option<&Value>) -> Result<Option<&Map<String, Value>>, String> {
    match permission {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(section)) => Ok(Some(section)),
        Some(_) => Err("permission: settings.json permission must be an object".into()),
    }
}

fn preset_spec(key: &str, entry: &Value) -> Result<PresetSpec, String> {
    let at = |field: &str| format!("permission: settings.json permission.presets.{key}{field}");
    let Value::Object(entry) = entry else {
        return Err(format!("{} must be an object", at("")));
    };
    let sandbox = match entry.get("sandbox") {
        Some(Value::String(mode)) => mode.parse::<SandboxMode>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        format!(
            "{} must be one of read-only, workspace-write, danger-full-access",
            at(".sandbox")
        )
    })?;
    let approval = match entry.get("approval") {
        Some(Value::String(policy)) => policy.parse::<ApprovalPolicy>().ok(),
        _ => None,
    }
    .ok_or_else(|| format!("{} must be one of ask, never", at(".approval")))?;
    let text = |field: &str| match entry.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{} must be a string", at(&format!(".{field}")))),
    };
    Ok(PresetSpec {
        sandbox,
        approval,
        name: text("name")?,
        description: text("description")?,
    })
}
