//! A session's permission, stored in its Pi session file.
//!
//! The TypeScript runtime kept three log-only events, `permission/preset`
//! (`packages/interaction/permission-presets/src/index.ts`), `sandbox/mode`
//! (`packages/sandbox/sandbox-policy/src/session-mode.ts`), and
//! `approval/policy` (`packages/interaction/user-approval/src/index.ts`),
//! and folded the last of each. Here each change is one Pi `custom` entry of
//! type [`PERMISSION_CUSTOM_TYPE`] whose data holds the changed fields
//! (`preset`, `sandbox`, `approval`), in that order. Pi's `custom` entries
//! never reach the model, as the TypeScript events did not; the session
//! file is the store, so a resumed session keeps its mode. The fold reads
//! the current branch.
//!
//! [`pin_initial_permission`] is `pinInitialPermission` and [`set_preset`]
//! is the preset service's `apply`, which `/permission` runs
//! ([`preset_change`] is the entry it writes). An approval's full-access
//! answer writes the same entry, in one group with its decision
//! ([`super::history`]).

use bake_sandbox::SandboxMode;
use serde_json::{Map, Value};

use super::approval::ApprovalPolicy;
use super::presets::{CUSTOM_PRESET, KnobDefaults, KnobState, PresetTable};
use crate::session::SessionManager;
use crate::session::entry::TypedEntry;

/// The `customType` of a permission entry.
pub const PERMISSION_CUSTOM_TYPE: &str = "bake.permission";

/// A session's folded permission facts (the `permissions` projection state).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionState {
    /// The last stored value of each knob.
    pub knobs: KnobState,
    /// Whether the session had entries before its permission was pinned:
    /// a resumed session, which keeps its composition defaults rather than
    /// taking the user's current default. TypeScript marks this with the
    /// `session/end-seed` boundary; a Pi session has none, so any earlier
    /// entry counts. The effect is the same: only an empty session takes
    /// the default preset whole.
    pub seeded: bool,
}

/// Folds one entry's data into `knobs`. A field of the wrong type or with
/// an unknown mode or policy is an error, so a damaged entry never reads as
/// a wider default.
pub fn apply_permission_data(knobs: &mut KnobState, data: Option<&Value>) -> Result<(), String> {
    let Some(Value::Object(data)) = data else {
        return Err(format!(
            "permission: a {PERMISSION_CUSTOM_TYPE} session entry has no data object"
        ));
    };
    let field = |name: &str| match data.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.as_str())),
        Some(_) => Err(format!(
            "permission: a {PERMISSION_CUSTOM_TYPE} session entry's {name} is not a string"
        )),
    };
    let preset = field("preset")?;
    let sandbox = field("sandbox")?
        .map(|mode| {
            mode.parse::<SandboxMode>().map_err(|_| {
                format!(
                    "permission: a {PERMISSION_CUSTOM_TYPE} session entry names unknown sandbox mode \"{mode}\""
                )
            })
        })
        .transpose()?;
    let approval = field("approval")?
        .map(|policy| {
            policy.parse::<ApprovalPolicy>().map_err(|_| {
                format!(
                    "permission: a {PERMISSION_CUSTOM_TYPE} session entry names unknown approval policy \"{policy}\""
                )
            })
        })
        .transpose()?;
    if let Some(preset) = preset {
        knobs.preset = Some(preset.to_owned());
    }
    if sandbox.is_some() {
        knobs.sandbox = sandbox;
    }
    if approval.is_some() {
        knobs.approval = approval;
    }
    Ok(())
}

/// The session's permission facts, folded over its current branch.
pub fn permission_state(session: &SessionManager) -> Result<PermissionState, String> {
    let mut knobs = KnobState::default();
    for entry in session.branch_entries(None) {
        if entry.entry_type() != "custom" {
            continue;
        }
        let Ok(TypedEntry::Custom(custom)) = entry.typed() else {
            continue;
        };
        if custom.custom_type == PERMISSION_CUSTOM_TYPE {
            apply_permission_data(&mut knobs, custom.data.as_ref())?;
        }
    }
    Ok(PermissionState {
        knobs,
        seeded: session.entry_count() > 0,
    })
}

/// One permission change: the fields one entry records, each `None` when
/// it does not change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionChange {
    /// The preset selected.
    pub preset: Option<String>,
    /// The new sandbox mode.
    pub sandbox: Option<SandboxMode>,
    /// The new approval policy.
    pub approval: Option<ApprovalPolicy>,
}

impl PermissionChange {
    /// Whether nothing changes.
    pub fn is_empty(&self) -> bool {
        self.preset.is_none() && self.sandbox.is_none() && self.approval.is_none()
    }

    /// The entry's `data`: the changed fields, in the order `preset`,
    /// `sandbox`, `approval`; `None` when nothing changes.
    pub fn to_data(&self) -> Option<Value> {
        let mut data = Map::new();
        if let Some(preset) = &self.preset {
            data.insert("preset".into(), preset.as_str().into());
        }
        if let Some(sandbox) = self.sandbox {
            data.insert("sandbox".into(), sandbox.as_str().into());
        }
        if let Some(approval) = self.approval {
            data.insert("approval".into(), approval.as_str().into());
        }
        (!data.is_empty()).then_some(Value::Object(data))
    }

    /// Writes the change as one entry; nothing when it is empty.
    pub fn append(&self, session: &mut SessionManager) -> Result<(), crate::session::SessionError> {
        match self.to_data() {
            Some(data) => session
                .append_custom_entry(PERMISSION_CUSTOM_TYPE, Some(data))
                .map(|_| ()),
            None => Ok(()),
        }
    }

    /// The change switching knobs `from` (their preset, sandbox mode, and
    /// approval policy) to preset `name` with `spec`'s knobs (the service's
    /// `apply`): the preset unless it is already the derived current value,
    /// then each knob that changes.
    pub fn to_preset(
        from: (&str, SandboxMode, ApprovalPolicy),
        name: &str,
        spec: &super::presets::PresetSpec,
    ) -> Self {
        let (current, sandbox, approval) = from;
        Self {
            preset: (current != name).then(|| name.to_owned()),
            sandbox: (spec.sandbox != sandbox).then_some(spec.sandbox),
            approval: (spec.approval != approval).then_some(spec.approval),
        }
    }
}

/// One change, written as one entry; nothing when every field is `None`.
fn append_change(
    session: &mut SessionManager,
    preset: Option<&str>,
    sandbox: Option<SandboxMode>,
    approval: Option<ApprovalPolicy>,
) -> Result<(), String> {
    PermissionChange {
        preset: preset.map(str::to_owned),
        sandbox,
        approval,
    }
    .append(session)
    .map_err(|error| error.to_string())
}

/// Fills every missing permission fact (`pinInitialPermission`). A fresh
/// session takes `default_preset` whole. A resumed one keeps its effective
/// values: it gains the preset they match, when one does, and the
/// composition defaults for missing knobs. Returns the session's knobs
/// afterwards.
pub fn pin_initial_permission(
    session: &mut SessionManager,
    table: &PresetTable,
    default_preset: &str,
    defaults: KnobDefaults,
) -> Result<KnobState, String> {
    let state = permission_state(session)?;
    let PermissionState { knobs, seeded } = &state;
    if knobs.preset.is_none() && knobs.sandbox.is_none() && knobs.approval.is_none() && !seeded {
        let spec = table.resolve(default_preset)?;
        append_change(
            session,
            Some(default_preset),
            Some(spec.sandbox),
            Some(spec.approval),
        )?;
        return Ok(KnobState {
            preset: Some(default_preset.to_owned()),
            sandbox: Some(spec.sandbox),
            approval: Some(spec.approval),
        });
    }
    let effective = table.derive(knobs, defaults);
    let preset = (knobs.preset.is_none() && effective != CUSTOM_PRESET).then_some(effective);
    let sandbox = knobs.sandbox.is_none().then_some(defaults.sandbox);
    let approval = knobs.approval.is_none().then_some(defaults.approval);
    append_change(session, preset.as_deref(), sandbox, approval)?;
    Ok(KnobState {
        preset: preset.or_else(|| knobs.preset.clone()),
        sandbox: knobs.sandbox.or(sandbox),
        approval: knobs.approval.or(approval),
    })
}

/// Switches the session to preset `name` (the service's `apply`): records
/// the preset unless it is already the derived current value, then each
/// knob that changes, as one entry. Selecting the current preset with
/// matching knobs writes nothing. An unknown name changes nothing.
pub fn set_preset(
    session: &mut SessionManager,
    table: &PresetTable,
    defaults: KnobDefaults,
    name: &str,
) -> Result<(), String> {
    preset_change(session, table, defaults, name)?
        .append(session)
        .map_err(|error| error.to_string())
}

/// The entry [`set_preset`] writes to switch the session to preset `name`;
/// empty when it already holds it. An unknown name is an error.
pub fn preset_change(
    session: &SessionManager,
    table: &PresetTable,
    defaults: KnobDefaults,
    name: &str,
) -> Result<PermissionChange, String> {
    let spec = table.resolve(name)?;
    let knobs = permission_state(session)?.knobs;
    let current = table.derive(&knobs, defaults);
    let (sandbox, approval) = knobs.effective(defaults);
    Ok(PermissionChange::to_preset(
        (&current, sandbox, approval),
        name,
        spec,
    ))
}

/// Switches one knob (`setSandboxMode`), writing nothing when it already
/// holds `mode`.
pub fn set_sandbox_mode(
    session: &mut SessionManager,
    defaults: KnobDefaults,
    mode: SandboxMode,
) -> Result<(), String> {
    let (current, _) = permission_state(session)?.knobs.effective(defaults);
    append_change(session, None, (current != mode).then_some(mode), None)
}

/// Switches the approval knob (`setApprovalPolicy`), writing nothing when
/// it already holds `policy`.
pub fn set_approval_policy(
    session: &mut SessionManager,
    defaults: KnobDefaults,
    policy: ApprovalPolicy,
) -> Result<(), String> {
    let (_, current) = permission_state(session)?.knobs.effective(defaults);
    append_change(session, None, None, (current != policy).then_some(policy))
}
