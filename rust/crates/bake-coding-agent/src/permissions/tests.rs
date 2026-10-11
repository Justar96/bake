//! Fixture tests for the permission layer, derived from Bake's TypeScript
//! specs; each names the case it follows:
//!
//! - `packages/interaction/permission-presets/tests/{permission-presets,projection,invariant}.spec.ts`
//! - `packages/interaction/user-approval/tests/{approval,invariant}.spec.ts`
//! - `packages/sandbox/sandbox-policy/tests/policy.spec.ts`
//! - `packages/shell/tool-bash/tests/tools.spec.ts` and
//!   `packages/fs/tool-fs/tests/tools.spec.ts` (the escalation cases)
//!
//! The tools themselves are tested in `tests/permissions.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bake_ai::AbortController;
use bake_sandbox::{EscalationOutcome, SandboxMode, SandboxPolicy};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::approval::ApprovalFuture;
use super::context::{permissions_section_text, sandbox_policy_sentence};
use super::presets::{AUTO_PRESET, PresetOption, settings_default_preset};
use super::state::{
    permission_state, pin_initial_permission, set_approval_policy, set_preset, set_sandbox_mode,
};
use super::*;
use crate::session::NewSessionOptions;

const WW: SandboxMode = SandboxMode::WorkspaceWrite;
const RO: SandboxMode = SandboxMode::ReadOnly;
const DFA: SandboxMode = SandboxMode::DangerFullAccess;
const ASK: ApprovalPolicy = ApprovalPolicy::Ask;
const NEVER: ApprovalPolicy = ApprovalPolicy::Never;

fn defaults() -> KnobDefaults {
    KnobDefaults::default()
}

fn spec(sandbox: SandboxMode, approval: ApprovalPolicy) -> PresetSpec {
    PresetSpec {
        sandbox,
        approval,
        name: None,
        description: None,
    }
}

fn session() -> SessionManager {
    SessionManager::in_memory(
        Some("/session-project"),
        NewSessionOptions::default(),
        Vec::new(),
    )
    .expect("session")
}

/// The same session, as a later run opens it.
fn resumed(session: &SessionManager) -> SessionManager {
    SessionManager::in_memory(
        Some("/session-project"),
        NewSessionOptions::default(),
        session.file_entries().to_vec(),
    )
    .expect("resumed")
}

/// The data of every permission entry, in order.
fn permission_entries(session: &SessionManager) -> Vec<Value> {
    session
        .entries()
        .into_iter()
        .filter(|entry| entry.entry_type() == "custom")
        .filter_map(|entry| match entry.typed() {
            Ok(crate::session::entry::TypedEntry::Custom(custom))
                if custom.custom_type == PERMISSION_CUSTOM_TYPE =>
            {
                custom.data
            }
            _ => None,
        })
        .collect()
}

fn append(session: &mut SessionManager, data: Value) {
    session
        .append_custom_entry(PERMISSION_CUSTOM_TYPE, Some(data))
        .expect("append");
}

fn current(session: &SessionManager, table: &PresetTable) -> String {
    table.derive(&permission_state(session).expect("state").knobs, defaults())
}

// permission-presets.spec: "advertises the preset table in declaration
// order and resolves bundles".
#[test]
fn the_default_table_is_the_typescript_table() {
    let table = PresetTable::default();
    assert_eq!(table.names(), ["workspace-write", "danger-full-access"]);
    let danger = table.resolve("danger-full-access").expect("known");
    assert_eq!((danger.sandbox, danger.approval), (DFA, NEVER));
    let write = table.resolve("workspace-write").expect("known");
    assert_eq!((write.sandbox, write.approval), (WW, ASK));
    assert_eq!(
        table.resolve("plan"),
        Err(
            "permission: unknown preset \"plan\" (known: workspace-write, danger-full-access)"
                .into()
        )
    );
}

// permission-presets.spec: "optionOf() presents shipped labels/descriptions,
// falls back to the raw key, and fixes custom".
#[test]
fn options_present_labels_and_custom() {
    let table = PresetTable::default();
    assert_eq!(
        table.option_of("danger-full-access"),
        Ok(PresetOption {
            value: "danger-full-access".into(),
            name: "danger-full-access".into(),
            description: Some("Full file access without approval prompts.".into()),
        })
    );
    assert_eq!(
        table.option_of("workspace-write").map(|option| option.description),
        Ok(Some("Write inside the workspace and permitted temporary directories; wider retries require approval.".into()))
    );
    assert_eq!(
        table.option_of("custom"),
        Ok(PresetOption {
            value: "custom".into(),
            name: "Custom".into(),
            description: Some(
                "Current sandbox and approval settings do not match a preset.".into()
            ),
        })
    );
    let bare = PresetTable::new(vec![("plain".into(), spec(WW, ASK))]).expect("table");
    assert_eq!(
        bare.option_of("plain"),
        Ok(PresetOption {
            value: "plain".into(),
            name: "plain".into(),
            description: None,
        })
    );
    assert!(table.option_of("plan").is_err());
}

// permission-presets.spec: "rejects a table entry named custom" and
// "reserves auto for an integration contribution".
#[test]
fn reserved_names_are_refused() {
    assert_eq!(
        PresetTable::new(vec![("custom".into(), spec(RO, ASK))]),
        Err("permission: \"custom\" is reserved for the derived not-a-preset state and cannot name a table entry".into())
    );
    assert_eq!(
        PresetTable::new(vec![(AUTO_PRESET.into(), spec(DFA, NEVER))]),
        Err("permission: \"auto\" is reserved and cannot name a configured preset".into())
    );
    let settings =
        json!({ "presets": { "auto": { "sandbox": "danger-full-access", "approval": "never" } } });
    assert!(PresetTable::from_settings(Some(&settings)).is_err());
}

// Bake: the table under `settings.json` `permission.presets`, in file
// order, and malformed tables refused.
#[test]
fn the_table_reads_from_settings() {
    assert_eq!(PresetTable::from_settings(None), Ok(PresetTable::default()));
    assert_eq!(
        PresetTable::from_settings(Some(&json!({}))),
        Ok(PresetTable::default())
    );
    let settings = json!({ "presets": {
        "read-only": { "sandbox": "read-only", "approval": "ask" },
        "workspace-write": { "sandbox": "workspace-write", "approval": "ask", "name": "Workspace", "description": "d" },
    }, "defaultPreset": "read-only" });
    let table = PresetTable::from_settings(Some(&settings)).expect("table");
    assert_eq!(table.names(), ["read-only", "workspace-write"]);
    assert_eq!(
        table.spec("workspace-write"),
        Some(&PresetSpec {
            sandbox: WW,
            approval: ASK,
            name: Some("Workspace".into()),
            description: Some("d".into()),
        })
    );
    assert_eq!(
        settings_default_preset(Some(&settings)),
        Ok(Some("read-only".into()))
    );
    for (bad, message) in [
        (
            json!([]),
            "permission: settings.json permission must be an object",
        ),
        (
            json!({ "presets": [] }),
            "permission: settings.json permission.presets must be an object",
        ),
        (
            json!({ "presets": { "x": 1 } }),
            "permission: settings.json permission.presets.x must be an object",
        ),
        (
            json!({ "presets": { "x": { "sandbox": "root", "approval": "ask" } } }),
            "permission: settings.json permission.presets.x.sandbox must be one of read-only, workspace-write, danger-full-access",
        ),
        (
            json!({ "presets": { "x": { "sandbox": "read-only" } } }),
            "permission: settings.json permission.presets.x.approval must be one of ask, never",
        ),
        (
            json!({ "presets": { "x": { "sandbox": "read-only", "approval": "ask", "name": 3 } } }),
            "permission: settings.json permission.presets.x.name must be a string",
        ),
    ] {
        assert_eq!(PresetTable::from_settings(Some(&bad)), Err(message.into()));
    }
    assert!(settings_default_preset(Some(&json!({ "defaultPreset": 1 }))).is_err());
}

// permission-presets.spec: "current() derives from the effective knobs" and
// "a knob state matching no table entry derives custom".
#[test]
fn derive_follows_the_effective_knobs() {
    let table = PresetTable::default();
    assert_eq!(
        table.derive(&KnobState::default(), defaults()),
        "workspace-write"
    );
    let custom = KnobState {
        sandbox: Some(RO),
        ..KnobState::default()
    };
    assert_eq!(table.derive(&custom, defaults()), CUSTOM_PRESET);
    let danger = KnobState {
        sandbox: Some(DFA),
        approval: Some(NEVER),
        ..KnobState::default()
    };
    assert_eq!(table.derive(&danger, defaults()), "danger-full-access");
    // "composition defaults outside the table still derive custom".
    let never = KnobDefaults {
        sandbox: WW,
        approval: NEVER,
    };
    assert_eq!(table.derive(&KnobState::default(), never), CUSTOM_PRESET);
}

// permission-presets.spec: "the fold breaks bundle ties; a stale fold no
// longer matching falls back to table order".
#[test]
fn a_matching_selection_breaks_ties() {
    let table = PresetTable::new(vec![
        ("workspace-write".into(), spec(WW, ASK)),
        ("agentish".into(), spec(WW, ASK)),
        ("danger-full-access".into(), spec(DFA, NEVER)),
    ])
    .expect("table");
    let mut session = session();
    set_preset(&mut session, &table, defaults(), "agentish").expect("set");
    assert_eq!(current(&session, &table), "agentish");
    append(&mut session, json!({ "approval": "never" }));
    append(&mut session, json!({ "sandbox": "danger-full-access" }));
    assert_eq!(current(&session, &table), "danger-full-access");
}

// permission-presets.spec: "requires an explicit default when composition
// defaults match no preset"; Bake: the default is checked against the table.
#[test]
fn the_default_preset_resolves_against_the_table() {
    let table = PresetTable::default();
    assert_eq!(
        table.default_preset(None, defaults()),
        Ok("workspace-write".into())
    );
    assert_eq!(
        table.default_preset(Some("danger-full-access"), defaults()),
        Ok("danger-full-access".into())
    );
    let never = KnobDefaults {
        sandbox: WW,
        approval: NEVER,
    };
    assert_eq!(
        table.default_preset(None, never),
        Err("permission: composed sandbox and approval defaults match no preset; configure defaultPreset explicitly".into())
    );
    assert!(table.default_preset(Some("yolo"), defaults()).is_err());
}

// permission-presets.spec: "set() writes through: one preset event plus
// both knob events" (one entry here) and "set() to the current preset is a
// no-op when the knobs already match".
#[test]
fn set_writes_only_what_changes() {
    let table = PresetTable::default();
    let mut session = session();
    set_preset(&mut session, &table, defaults(), "workspace-write").expect("no-op");
    assert!(permission_entries(&session).is_empty());
    set_preset(&mut session, &table, defaults(), "danger-full-access").expect("set");
    assert_eq!(
        permission_entries(&session),
        [
            json!({ "preset": "danger-full-access", "sandbox": "danger-full-access", "approval": "never" })
        ]
    );
    set_preset(&mut session, &table, defaults(), "danger-full-access").expect("again");
    assert_eq!(permission_entries(&session).len(), 1);
    assert!(set_preset(&mut session, &table, defaults(), "yolo").is_err());
    assert_eq!(permission_entries(&session).len(), 1);
}

// permission-presets.spec: "re-asserting a preset from a drifted (custom)
// state re-records the choice and repairs the knob".
#[test]
fn reasserting_from_a_drifted_state_repairs_the_knob() {
    let table = PresetTable::default();
    let mut session = session();
    set_preset(&mut session, &table, defaults(), "danger-full-access").expect("set");
    set_sandbox_mode(&mut session, defaults(), RO).expect("drift");
    assert_eq!(current(&session, &table), CUSTOM_PRESET);
    set_preset(&mut session, &table, defaults(), "danger-full-access").expect("repair");
    assert_eq!(
        permission_entries(&session)[2..],
        [json!({ "preset": "danger-full-access", "sandbox": "danger-full-access" })]
    );
}

// sandbox-policy.spec: "setSandboxMode appends exactly one sandbox/mode
// event per switch"; user-approval.spec: "folds to the last event".
#[test]
fn knob_switches_append_one_entry_each() {
    let mut session = session();
    set_sandbox_mode(&mut session, defaults(), DFA).expect("mode");
    set_approval_policy(&mut session, defaults(), NEVER).expect("never");
    set_approval_policy(&mut session, defaults(), ASK).expect("ask");
    assert_eq!(
        permission_entries(&session),
        [
            json!({ "sandbox": "danger-full-access" }),
            json!({ "approval": "never" }),
            json!({ "approval": "ask" }),
        ]
    );
    let knobs = permission_state(&session).expect("state").knobs;
    assert_eq!((knobs.sandbox, knobs.approval), (Some(DFA), Some(ASK)));
}

// permission-presets.spec: "folds the latest preset selection and steps over
// unrelated events"; user-approval.spec "folds to the last event".
#[test]
fn the_fold_steps_over_other_entries() {
    let mut session = session();
    append(&mut session, json!({ "preset": "danger-full-access" }));
    session
        .append_custom_entry("other.extension", Some(json!({ "sandbox": "read-only" })))
        .expect("other");
    append(&mut session, json!({ "preset": "workspace-write" }));
    session.append_session_info("named").expect("info");
    let knobs = permission_state(&session).expect("state").knobs;
    assert_eq!(knobs.preset.as_deref(), Some("workspace-write"));
    assert_eq!(knobs.sandbox, None);
}

// user-approval invariant.spec: "approval/policy carries unknown policy";
// sandbox-policy rejects an unknown mode. A damaged entry fails closed
// instead of reading as the default.
#[test]
fn damaged_entries_are_errors() {
    for data in [
        json!({ "sandbox": "root" }),
        json!({ "approval": "sometimes" }),
        json!({ "sandbox": 1 }),
        json!("workspace-write"),
    ] {
        let mut session = session();
        append(&mut session, data);
        assert!(permission_state(&session).is_err());
    }
    let mut missing = session();
    missing
        .append_custom_entry(PERMISSION_CUSTOM_TYPE, None)
        .expect("append");
    assert!(permission_state(&missing).is_err());
}

// permission-presets.spec (new-session default): "pins the current setting
// into each new session without changing earlier sessions".
#[test]
fn a_new_session_pins_the_default_and_keeps_it() {
    let table = PresetTable::default();
    let mut first = session();
    pin_initial_permission(&mut first, &table, "workspace-write", defaults()).expect("pin");
    assert_eq!(
        permission_entries(&first),
        [json!({ "preset": "workspace-write", "sandbox": "workspace-write", "approval": "ask" })]
    );
    let mut second = session();
    pin_initial_permission(&mut second, &table, "danger-full-access", defaults()).expect("pin");
    assert_eq!(current(&first, &table), "workspace-write");
    assert_eq!(current(&second, &table), "danger-full-access");

    // A resumed session keeps its mode whatever the default now is.
    let mut again = resumed(&second);
    let before = permission_entries(&again).len();
    pin_initial_permission(&mut again, &table, "workspace-write", defaults()).expect("resume");
    assert_eq!(permission_entries(&again).len(), before);
    assert_eq!(current(&again, &table), "danger-full-access");
}

// permission-presets.spec: "preserves a seeded legacy session instead of
// applying the latest user default" and "preserves existing knob overrides".
#[test]
fn a_resumed_session_keeps_its_composition_defaults() {
    let table = PresetTable::default();
    let mut legacy = session();
    legacy
        .append_session_info("before permissions")
        .expect("entry");
    let mut legacy = resumed(&legacy);
    pin_initial_permission(&mut legacy, &table, "danger-full-access", defaults()).expect("pin");
    assert_eq!(current(&legacy, &table), "workspace-write");
    assert_eq!(
        permission_entries(&legacy),
        [json!({ "preset": "workspace-write", "sandbox": "workspace-write", "approval": "ask" })]
    );

    let mut custom = session();
    append(&mut custom, json!({ "sandbox": "read-only" }));
    append(&mut custom, json!({ "approval": "never" }));
    let mut custom = resumed(&custom);
    pin_initial_permission(&mut custom, &table, "workspace-write", defaults()).expect("pin");
    assert_eq!(permission_entries(&custom).len(), 2);
    assert_eq!(current(&custom, &table), CUSTOM_PRESET);
}

// permission-presets.spec: "fills only missing legacy facts and preserves an
// unmatched seeded combination" and "materializes ask when a legacy seed
// omits the policy".
#[test]
fn a_resumed_session_gains_only_missing_facts() {
    let table = PresetTable::default();
    let mut partial = session();
    append(&mut partial, json!({ "sandbox": "workspace-write" }));
    let mut partial = resumed(&partial);
    pin_initial_permission(&mut partial, &table, "danger-full-access", defaults()).expect("pin");
    assert_eq!(
        permission_entries(&partial).last(),
        Some(&json!({ "preset": "workspace-write", "approval": "ask" }))
    );
}

// Bake: `--permission`, `--sandbox`, and `--approval` at startup, the
// imported default, and a stale imported name.
#[test]
fn startup_settles_the_permission() {
    let mut fresh = session();
    let settled = prepare_session_permission(
        &mut fresh,
        &PermissionInputs {
            imported_default_preset: Some("danger-full-access"),
            ..PermissionInputs::default()
        },
    )
    .expect("settled");
    assert_eq!(
        (settled.sandbox, settled.approval, settled.preset.as_str()),
        (DFA, NEVER, "danger-full-access")
    );
    assert!(settled.warnings.is_empty());

    // settings.json wins over the import.
    let mut configured = session();
    let settings = json!({ "defaultPreset": "workspace-write" });
    let settled = prepare_session_permission(
        &mut configured,
        &PermissionInputs {
            settings: Some(&settings),
            imported_default_preset: Some("danger-full-access"),
            ..PermissionInputs::default()
        },
    )
    .expect("settled");
    assert_eq!(settled.preset, "workspace-write");

    let mut stale = session();
    let settled = prepare_session_permission(
        &mut stale,
        &PermissionInputs {
            imported_default_preset: Some("read-only"),
            ..PermissionInputs::default()
        },
    )
    .expect("settled");
    assert_eq!(settled.preset, "workspace-write");
    assert_eq!(
        settled.warnings,
        [
            "Ignoring the imported default permission preset \"read-only\": it names no preset (available: workspace-write, danger-full-access)"
        ]
    );

    // `--permission` pins a new session once; on a resumed one it switches.
    let mut flagged = session();
    prepare_session_permission(
        &mut flagged,
        &PermissionInputs {
            preset: Some("danger-full-access"),
            ..PermissionInputs::default()
        },
    )
    .expect("settled");
    assert_eq!(permission_entries(&flagged).len(), 1);
    let mut back = resumed(&flagged);
    let settled = prepare_session_permission(
        &mut back,
        &PermissionInputs {
            preset: Some("workspace-write"),
            sandbox: Some(RO),
            ..PermissionInputs::default()
        },
    )
    .expect("settled");
    assert_eq!(
        (settled.sandbox, settled.approval, settled.preset.as_str()),
        (RO, ASK, CUSTOM_PRESET)
    );
    assert_eq!(
        permission_entries(&back)[1..],
        [
            json!({ "preset": "workspace-write", "sandbox": "workspace-write", "approval": "ask" }),
            json!({ "sandbox": "read-only" }),
        ]
    );
    let mut unknown = session();
    assert_eq!(
        prepare_session_permission(
            &mut unknown,
            &PermissionInputs {
                preset: Some("yolo"),
                ..PermissionInputs::default()
            },
        ),
        Err("Unknown preset \"yolo\" (available: workspace-write, danger-full-access)".into())
    );
    assert!(permission_entries(&unknown).is_empty());
}

// Bake (D25): `permission.defaultPreset` from the 0.3 `settings.yaml`.
#[test]
fn the_default_preset_imports_from_the_old_home() {
    let home = crate::test_support::TempDir::new("permission-import");
    assert_eq!(import::import_default_preset(home.path()), Ok(None));
    let settings = "agent-default-model:\n  provider: x\n  model: y\npermission:\n  defaultPreset: danger-full-access\n";
    crate::test_support::write(&home.join("settings.yaml"), settings);
    assert_eq!(
        import::import_default_preset(home.path()),
        Ok(Some("danger-full-access".into()))
    );
    // Read-only: the file is unchanged.
    assert_eq!(
        std::fs::read_to_string(home.join("settings.yaml")).expect("read"),
        settings
    );
    crate::test_support::write(&home.join("settings.yaml"), "permission: []\n");
    assert!(import::import_default_preset(home.path()).is_err());
    crate::test_support::write(
        &home.join("settings.yaml"),
        "permission:\n  defaultPreset: 3\n",
    );
    assert!(import::import_default_preset(home.path()).is_err());
    crate::test_support::write(&home.join("settings.yaml"), "other: 1\n");
    assert_eq!(import::import_default_preset(home.path()), Ok(None));
}

// sandbox-policy.spec: "renders the exact %s policy without a capability
// inventory", with the root spelled as the session recorded it.
#[test]
fn the_policy_sentences_are_the_typescript_ones() {
    let root = "/projects/../projects/current";
    let sentence = |mode| sandbox_policy_sentence(&SandboxPolicy::new(mode, root));
    assert_eq!(
        sentence(RO),
        "File sandbox: read-only. Sandboxed tools cannot modify files. Still attempt required changes normally, and follow the guidance any denial returns."
    );
    assert_eq!(
        sentence(WW),
        "File sandbox: workspace-write. Sandboxed tools can modify files under \"/projects/../projects/current\", and possibly in some temporary directories."
    );
    assert_eq!(
        sentence(DFA),
        "File sandbox: danger-full-access. File changes are not restricted."
    );
    assert_eq!(
        sandbox_policy_sentence(&SandboxPolicy::new(WW, "C:\\work \"q\"")),
        "File sandbox: workspace-write. Sandboxed tools can modify files under \"C:\\\\work \\\"q\\\"\", and possibly in some temporary directories."
    );
    // user-approval.spec: "contributes the complete current ask or never
    // policy as cache-safe context".
    assert_eq!(
        permissions_section_text(&SandboxPolicy::new(DFA, root), NEVER),
        "File sandbox: danger-full-access. File changes are not restricted.\n\nApproval prompts: disabled. Actions that need approval are rejected automatically, so do not request sandbox escalation with `sandbox_permissions`."
    );
    assert_eq!(
        approval::approval_policy_sentence(ASK),
        "Approval prompts: enabled. Actions that need approval request it before running, and fail if no one can answer."
    );
}

// user-approval.spec: "rejects a policy outside the closed vocabulary".
#[test]
fn approval_policies_parse_strictly() {
    assert_eq!("ask".parse::<ApprovalPolicy>(), Ok(ASK));
    assert_eq!("never".parse::<ApprovalPolicy>(), Ok(NEVER));
    assert_eq!(
        "sometimes"
            .parse::<ApprovalPolicy>()
            .map_err(|error| error.to_string()),
        Err("approval policy must be one of \"ask\" or \"never\"".into())
    );
}

// sandbox-policy.spec: "resolves each session mode and cwd together".
#[test]
fn the_standing_policy_carries_root_and_session() {
    let standing = StandingPermissions::new(
        WW,
        ASK,
        "workspace-write",
        "/projects/first",
        Some("s1".into()),
    );
    // The root is pinned canonical; a missing one keeps its spelling below
    // its existing prefix.
    let root = bake_sandbox::canonical_path(std::path::Path::new("/projects/first"));
    assert_eq!(
        standing.policy(),
        SandboxPolicy::new(WW, root).with_session_id("s1")
    );
    standing.set(RO, NEVER, CUSTOM_PRESET);
    assert_eq!(standing.policy().mode, RO);
    assert_eq!(standing.approval(), NEVER);
}

// ---- Approvals and the engine ----

/// Counts requests and answers with a fixed outcome.
struct Fixed {
    outcome: EscalationOutcome,
    asked: Arc<AtomicUsize>,
}

impl Approver for Fixed {
    fn request(
        &self,
        _request: ApprovalRequest,
        _cancel: Option<bake_ai::AbortSignal>,
    ) -> ApprovalFuture {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(self.outcome))
    }
}

/// Signals when asked, then never answers; records when its answer is
/// dropped.
struct Pending {
    entered: Arc<Notify>,
    dropped: Arc<AtomicUsize>,
}

struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Approver for Pending {
    fn request(
        &self,
        _request: ApprovalRequest,
        _cancel: Option<bake_ai::AbortSignal>,
    ) -> ApprovalFuture {
        let entered = Arc::clone(&self.entered);
        let flag = DropFlag(Arc::clone(&self.dropped));
        Box::pin(async move {
            let _flag = flag;
            entered.notify_one();
            std::future::pending::<EscalationOutcome>().await
        })
    }
}

struct Panics;

impl Approver for Panics {
    fn request(
        &self,
        _request: ApprovalRequest,
        _cancel: Option<bake_ai::AbortSignal>,
    ) -> ApprovalFuture {
        Box::pin(async { panic!("answerer bug") })
    }
}

fn standing(mode: SandboxMode, approval: ApprovalPolicy) -> Arc<StandingPermissions> {
    Arc::new(StandingPermissions::new(
        mode,
        approval,
        "x",
        "/session-project",
        Some("sess".into()),
    ))
}

fn request() -> ApprovalRequest {
    ApprovalRequest {
        tool_name: "bash".into(),
        call_id: "call-1".into(),
        reason: "escalate sandbox to workspace-write: needed".into(),
    }
}

// user-approval.spec: "fails closed to unavailable when nobody listens", "a
// never config rejects deterministically without consulting any answerer",
// "a session override outranks the configured default", and "resolves the
// answer when the signal never aborts".
#[tokio::test]
async fn the_policy_decides_before_the_answerer() {
    let standing = standing(WW, ASK);
    let none = PolicyApprover::new(Arc::clone(&standing), Arc::new(NoApprover));
    assert_eq!(
        none.request(request(), None).await,
        EscalationOutcome::Unavailable
    );
    assert_eq!(
        NeverApprover.request(request(), None).await,
        EscalationOutcome::Rejected
    );
    let asked = Arc::new(AtomicUsize::new(0));
    let grants = PolicyApprover::new(
        Arc::clone(&standing),
        Arc::new(Fixed {
            outcome: EscalationOutcome::AllowedOnce,
            asked: Arc::clone(&asked),
        }),
    );
    let controller = AbortController::new();
    assert_eq!(
        grants.request(request(), Some(controller.signal())).await,
        EscalationOutcome::AllowedOnce
    );
    standing.set(WW, NEVER, CUSTOM_PRESET);
    assert_eq!(
        grants.request(request(), None).await,
        EscalationOutcome::Rejected
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

// user-approval.spec: "settles cancelled immediately on an already-aborted
// signal without asking anyone" and "resolves cancelled when the signal
// aborts mid-question and discards the late answer".
#[tokio::test]
async fn cancellation_withdraws_the_question() {
    let asked = Arc::new(AtomicUsize::new(0));
    let approver = PolicyApprover::new(
        standing(WW, ASK),
        Arc::new(Fixed {
            outcome: EscalationOutcome::AllowedOnce,
            asked: Arc::clone(&asked),
        }),
    );
    let controller = AbortController::new();
    controller.abort();
    assert_eq!(
        approver.request(request(), Some(controller.signal())).await,
        EscalationOutcome::Cancelled
    );
    assert_eq!(asked.load(Ordering::SeqCst), 0);

    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let approver = PolicyApprover::new(
        standing(WW, ASK),
        Arc::new(Pending {
            entered: Arc::clone(&entered),
            dropped: Arc::clone(&dropped),
        }),
    );
    let controller = AbortController::new();
    let pending = approver.request(request(), Some(controller.signal()));
    let (outcome, ()) = tokio::join!(pending, async {
        entered.notified().await;
        controller.abort();
    });
    assert_eq!(outcome, EscalationOutcome::Cancelled);
    // The withdrawn answer is gone once the request settles: no late answer.
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

// user-approval.spec: "contains a throwing answerer as unavailable".
#[tokio::test]
async fn a_panicking_answerer_fails_closed() {
    let approver = PolicyApprover::new(standing(WW, ASK), Arc::new(Panics));
    assert_eq!(
        approver.request(request(), None).await,
        EscalationOutcome::Unavailable
    );
}

fn engine(mode: SandboxMode, outcome: EscalationOutcome) -> (PermissionEngine, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let standing = standing(mode, ASK);
    let approver = PolicyApprover::new(
        Arc::clone(&standing),
        Arc::new(Fixed {
            outcome,
            asked: Arc::clone(&asked),
        }),
    );
    (
        PermissionEngine::standard(standing, Arc::new(approver)),
        asked,
    )
}

async fn resolve(
    engine: &PermissionEngine,
    args: Value,
    blank: BlankJustification,
) -> Result<SandboxPolicy, String> {
    let subject = match blank {
        BlankJustification::IgnoredAlone => "command",
        BlankJustification::Checked => "operation",
    };
    let call = ToolCall {
        tool_name: "bash",
        call_id: "call-1",
        args: &args,
        subject,
        blank_justification: blank,
    };
    engine.resolve(&call, None).await
}

const BASH: BlankJustification = BlankJustification::IgnoredAlone;
const FS: BlankJustification = BlankJustification::Checked;

// tool-bash.spec: "runs without escalation for justification %j" and
// "runs a repeated %s request without a reason or approval"; tool-fs.spec:
// "a plain write stamps the default mode with the calling session root" and
// "ignores a request that does not widen the standing mode".
#[tokio::test]
async fn requests_for_access_the_call_has_need_nothing() {
    let (engine, asked) = engine(WW, EscalationOutcome::AllowedOnce);
    let standing = SandboxPolicy::new(WW, "/session-project").with_session_id("sess");
    for args in [
        json!({ "command": "true" }),
        json!({ "command": "true", "justification": "" }),
        json!({ "command": "true", "justification": " \t\n\u{feff}" }),
        json!({ "command": "true", "sandbox_permissions": "workspace-write" }),
        json!({ "command": "true", "sandbox_permissions": "workspace-write", "justification": "" }),
        json!({ "command": "true", "sandbox_permissions": "read-only", "justification": "x" }),
    ] {
        assert_eq!(
            resolve(&engine, args.clone(), BASH).await,
            Ok(standing.clone()),
            "{args}"
        );
    }
    for args in [
        json!({ "path": "a" }),
        json!({ "path": "a", "sandbox_permissions": "workspace-write" }),
        json!({ "path": "a", "sandbox_permissions": "workspace-write", "justification": "" }),
    ] {
        assert_eq!(
            resolve(&engine, args.clone(), FS).await,
            Ok(standing.clone()),
            "{args}"
        );
    }
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

// tool-bash/src/index.ts and tool-fs/src/sandbox.ts: a composition with no
// sandboxing executor or filesystem refuses escalation arguments with its
// own text and passes a call without them. An engine without a standing
// layer is such a composition.
#[tokio::test]
async fn escalation_without_a_policy_names_the_missing_sandbox() {
    let engine = PermissionEngine::new(Arc::new(NoApprover)).with_layer(Arc::new(EscalationLayer));
    let args = json!({ "sandbox_permissions": "danger-full-access", "justification": "x" });
    assert_eq!(
        resolve(&engine, args.clone(), BASH).await,
        Err("sandbox_permissions is not available in this composition (no sandboxing executor to escalate)".into())
    );
    assert_eq!(
        resolve(&engine, args, FS).await,
        Err("sandbox_permissions is not available in this composition (no sandboxing filesystem to escalate)".into())
    );
    assert!(
        resolve(&engine, json!({}), BASH)
            .await
            .is_err_and(|text| text.starts_with("permission: no policy layer"))
    );
}

// tool-bash.spec: "advertises the sandbox fields and validates their
// pairing", "rejects an explicit mode with blank justification"; tool-fs.spec:
// "rejects the escalation argument pairing".
#[tokio::test]
async fn bad_pairings_are_refused_before_asking() {
    let (engine, asked) = engine(RO, EscalationOutcome::AllowedOnce);
    let cases = [
        (
            json!({ "sandbox_permissions": "workspace-write" }),
            BASH,
            "invalid escalation: sandbox_permissions requires a justification",
        ),
        (
            json!({ "justification": "why" }),
            BASH,
            "invalid escalation: justification is only valid together with sandbox_permissions",
        ),
        (
            json!({ "sandbox_permissions": "workspace-write", "justification": " \t\n" }),
            BASH,
            "invalid justification: expected a non-empty sentence",
        ),
        (
            json!({ "sandbox_permissions": "workspace-write", "justification": "" }),
            FS,
            "invalid justification: expected a non-empty sentence",
        ),
        // tool-fs keeps a lone blank justification and refuses it.
        (
            json!({ "justification": " " }),
            FS,
            "invalid escalation: justification is only valid together with sandbox_permissions",
        ),
        (
            json!({ "sandbox_permissions": 3, "justification": "why" }),
            BASH,
            "invalid escalation: sandbox_permissions must be a string",
        ),
    ];
    for (args, blank, message) in cases {
        assert_eq!(
            resolve(&engine, args.clone(), blank).await,
            Err(message.into()),
            "{args}"
        );
    }
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

// tool-bash.spec: "rejects injected escalation ... not strictly wider".
#[tokio::test]
async fn an_unknown_target_is_not_wider() {
    let (engine, asked) = engine(WW, EscalationOutcome::AllowedOnce);
    assert_eq!(
        resolve(&engine, json!({ "sandbox_permissions": "root", "justification": "why" }), BASH).await,
        Err("sandbox escalation to \"root\" is not strictly wider than this call's current \"workspace-write\" mode".into())
    );
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

// tool-bash.spec: "runs a granted foreground ... call under the approved
// mode" and "uses the session override for ordinary calls and evaluates
// widening against it"; sandbox-policy.spec: "lets an approved mode outrank
// the session mode while retaining its root"; tool-fs.spec: "an approved
// escalation stamps the granted mode onto that write".
#[tokio::test]
async fn an_approved_widening_applies_to_that_call_only() {
    let (engine, asked) = engine(RO, EscalationOutcome::AllowedOnce);
    let granted = resolve(
        &engine,
        json!({ "sandbox_permissions": "danger-full-access", "justification": "the test needs it" }),
        FS,
    )
    .await
    .expect("granted");
    assert_eq!(
        granted,
        SandboxPolicy::new(DFA, "/session-project").with_session_id("sess")
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(
        resolve(&engine, json!({}), FS)
            .await
            .map(|policy| policy.mode),
        Ok(RO)
    );
}

// tool-bash.spec: "maps an approval %s to its distinct failure" and "fails
// closed when approval cannot be routed"; tool-fs.spec: "a rejected
// escalation fails closed with its own text".
#[tokio::test]
async fn each_refused_outcome_has_its_text() {
    let args = json!({ "sandbox_permissions": "danger-full-access", "justification": "why" });
    for (outcome, blank, message) in [
        (
            EscalationOutcome::Rejected,
            BASH,
            "the user rejected escalating this command to \"danger-full-access\"",
        ),
        (
            EscalationOutcome::Rejected,
            FS,
            "the user rejected escalating this operation to \"danger-full-access\"",
        ),
        (
            EscalationOutcome::Cancelled,
            BASH,
            "approval for escalating to \"danger-full-access\" was cancelled",
        ),
        (
            EscalationOutcome::Unavailable,
            FS,
            "sandbox escalation to \"danger-full-access\" requires approval, but no approval channel is available",
        ),
    ] {
        let (engine, _) = engine(WW, outcome);
        assert_eq!(
            resolve(&engine, args.clone(), blank).await,
            Err(message.into())
        );
    }
    // Headless `ask`: no answerer, so the request is unavailable.
    let standing = standing(WW, ASK);
    let headless = PermissionEngine::standard(
        Arc::clone(&standing),
        Arc::new(PolicyApprover::new(
            Arc::clone(&standing),
            Arc::new(NoApprover),
        )),
    );
    assert_eq!(
        resolve(&headless, args.clone(), BASH).await,
        Err("sandbox escalation to \"danger-full-access\" requires approval, but no approval channel is available".into())
    );
    // Headless `never`: rejected.
    standing.set(WW, NEVER, CUSTOM_PRESET);
    assert_eq!(
        resolve(&headless, args, BASH).await,
        Err("the user rejected escalating this command to \"danger-full-access\"".into())
    );
}

// Bake: an approval withdrawn by the call's abort is `cancelled`.
#[tokio::test]
async fn an_aborted_call_cancels_its_approval() {
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let standing = standing(WW, ASK);
    let approver = PolicyApprover::new(
        Arc::clone(&standing),
        Arc::new(Pending {
            entered: Arc::clone(&entered),
            dropped: Arc::clone(&dropped),
        }),
    );
    let engine = PermissionEngine::standard(standing, Arc::new(approver));
    let controller = AbortController::new();
    let args = json!({ "sandbox_permissions": "danger-full-access", "justification": "why" });
    let call = ToolCall {
        tool_name: "bash",
        call_id: "c",
        args: &args,
        subject: "command",
        blank_justification: BASH,
    };
    let signal = controller.signal();
    let (outcome, ()) = tokio::join!(engine.resolve(&call, Some(&signal)), async {
        entered.notified().await;
        controller.abort();
    });
    assert_eq!(
        outcome,
        Err("approval for escalating to \"danger-full-access\" was cancelled".into())
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

/// A layer that refuses one tool by name: the shape a per-tool rule takes.
struct DenyTool(&'static str);

impl PolicyLayer for DenyTool {
    fn name(&self) -> &str {
        "deny-tool"
    }

    fn evaluate<'a>(
        &'a self,
        call: &'a ToolCall<'a>,
        _policy: Option<&'a SandboxPolicy>,
    ) -> bake_agent::BoxFuture<'a, LayerDecision> {
        let decision = if call.tool_name == self.0 {
            LayerDecision::Refuse(format!("{} is not allowed here", self.0))
        } else {
            LayerDecision::Pass
        };
        Box::pin(std::future::ready(decision))
    }
}

/// A layer that narrows every call to read-only.
struct Narrow;

impl PolicyLayer for Narrow {
    fn name(&self) -> &str {
        "narrow"
    }

    fn evaluate<'a>(
        &'a self,
        _call: &'a ToolCall<'a>,
        policy: Option<&'a SandboxPolicy>,
    ) -> bake_agent::BoxFuture<'a, LayerDecision> {
        let decision = match policy {
            Some(policy) => LayerDecision::SetPolicy(policy.clone().with_mode(RO)),
            None => LayerDecision::Pass,
        };
        Box::pin(std::future::ready(decision))
    }
}

// Bake: layers run in order; a refusal stops the rest; a later layer sees
// the earlier policy; and an engine whose layers set no policy refuses.
#[tokio::test]
async fn layers_compose_in_order() {
    let (mut engine, asked) = engine(WW, EscalationOutcome::AllowedOnce);
    assert_eq!(engine.layer_names(), ["standing-mode", "escalation"]);
    engine.insert_layer(1, Arc::new(Narrow));
    engine.insert_layer(usize::MAX, Arc::new(DenyTool("bash")));
    assert_eq!(
        engine.layer_names(),
        ["standing-mode", "narrow", "escalation", "deny-tool"]
    );
    // Narrowed to read-only, a workspace-write request is now a widening.
    assert_eq!(
        resolve(
            &engine,
            json!({ "sandbox_permissions": "workspace-write", "justification": "x" }),
            FS
        )
        .await,
        Err("bash is not allowed here".into())
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    let empty = PermissionEngine::new(Arc::new(NoApprover));
    assert_eq!(
        resolve(&empty, json!({}), FS).await,
        Err("permission: no policy layer resolved a sandbox policy for this bash call".into())
    );
    assert_eq!(
        resolve(
            &empty,
            json!({ "sandbox_permissions": "workspace-write", "justification": "x" }),
            FS
        )
        .await,
        Err("permission: no policy layer resolved a sandbox policy for this bash call".into())
    );
}
