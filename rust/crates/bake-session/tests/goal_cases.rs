//! Runs every shared case in `conformance/session/goal-cases.json` through
//! `restore_plain_log` and then `goal_projection`, over the same bytes the
//! TypeScript spec folds. Every case must restore. A case's expected outcome
//! is its `rust` override when present, a native limit at the seq Rust
//! refuses, otherwise the hand-written state the TypeScript fold also meets.
//! Nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    GoalLimit, GoalProjectionState, GoalRefusal, PathPlatform, goal_projection, restore_plain_log,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/goal-cases";
const ORACLE: &str = "goalProjectionDefinition.init and applyGoalProjection in packages/goal/goal/src/index.ts, folded over each case's parsed rows and their interruptedTurnClosers";
/// The capture and its size; the TypeScript spec checks its SHA-256.
const LOGS: [(&str, &str, usize); 1] = [(
    "tool-call-turn",
    "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl",
    4533,
)];
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 113;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [(&str, GoalLimit); 2] = [
    ("number", GoalLimit::Number),
    ("version-diagnostic", GoalLimit::VersionDiagnostic),
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn object<'a>(value: &'a Value, context: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected an object"))
}

fn keys(fields: &Map<String, Value>) -> BTreeSet<&str> {
    fields.keys().map(String::as_str).collect()
}

fn text<'a>(value: &'a Value, context: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a string"))
}

fn index(value: &Value, context: &str) -> usize {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| panic!("{context}: expected a count"))
}

struct Case {
    id: String,
    log: Vec<u8>,
    row_count: usize,
    entry: Map<String, Value>,
}

/// Apply a case's row edits to its capture, as the TypeScript spec does.
fn build(entry: &Map<String, Value>, id: &str) -> (Vec<u8>, usize) {
    let name = text(&entry["log"], id);
    let (_, path, size) = LOGS
        .iter()
        .find(|(log, _, _)| *log == name)
        .unwrap_or_else(|| panic!("{id}: unknown log {name}"));
    let source = std::fs::read_to_string(repo_path(path)).expect("read capture");
    assert_eq!(source.len(), *size, "{path} changed");
    let mut rows: Vec<String> = source
        .strip_suffix('\n')
        .expect("final LF")
        .split('\n')
        .map(str::to_owned)
        .collect();
    let header = rows.remove(0);
    for edit in entry["edits"].as_array().expect("edits") {
        let edit = object(edit, id);
        let line = |key: &str| {
            let value = text(&edit[key], id);
            assert!(!value.contains('\n'), "{id}: {key} holds an LF");
            value.to_owned()
        };
        match keys(edit).into_iter().collect::<Vec<_>>().as_slice() {
            ["truncate"] => {
                let count = index(&edit["truncate"], id);
                assert!(count <= rows.len(), "{id}: truncate past the end");
                rows.truncate(count);
            }
            ["append"] => rows.push(line("append")),
            ["row", "text"] => {
                let row = index(&edit["row"], id);
                assert!(row < rows.len(), "{id}: no row {row}");
                rows[row] = line("text");
            }
            other => panic!("{id}: invalid edit {other:?}"),
        }
    }
    let mut log = String::new();
    for line in std::iter::once(&header).chain(&rows) {
        log.push_str(line);
        log.push('\n');
    }
    (log.into_bytes(), rows.len())
}

fn load() -> Vec<Case> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/goal-cases.json")).expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "logs", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 2);
    assert!(
        table["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    assert_eq!(table["oracle"], ORACLE);
    let logs = object(&table["logs"], "logs");
    assert_eq!(logs.len(), LOGS.len());
    for (name, path, _) in LOGS {
        assert_eq!(logs[name], path);
    }
    table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|entry| {
            let entry = object(entry, "case").clone();
            let id = text(&entry["id"], "case id").to_owned();
            let allowed = BTreeSet::from(["id", "log", "edits", "ts", "rust", "note"]);
            assert!(keys(&entry).is_subset(&allowed), "{id}: unknown keys");
            let (log, row_count) = build(&entry, &id);
            Case {
                id,
                log,
                row_count,
                entry,
            }
        })
        .collect()
}

/// The folded state in the table's form, `GoalProjectionState` on the wire.
fn state_value(state: &GoalProjectionState) -> Value {
    let current = state.current.as_ref().map(|current| {
        let goal = &current.goal;
        let mut snapshot = json!({
            "id": goal.id,
            "revision": goal.revision,
            "objective": goal.objective,
            "phase": goal.phase.as_str(),
            "maxGoalRounds": goal.max_goal_rounds,
        });
        if let Some(reason) = &goal.blocked_reason {
            snapshot["blockedReason"] = json!({"code": reason.code, "message": reason.message});
        }
        json!({
            "goal": snapshot,
            "roundsStarted": current.rounds_started,
            "createdAt": current.created_at,
            "updatedAt": current.updated_at,
        })
    });
    json!({
        "current": current,
        "seenGoalIds": state.seen_goal_ids,
        "failure": state.failure,
    })
}

#[test]
fn shared_cases_fold_like_the_goal_projection() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut limits = BTreeSet::new();
    for case in &cases {
        let id = &case.id;
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: every case restores, got {refusal:?}"));
        // Both arms fold the same rows: no torn tail is left out.
        assert_eq!(restored.stored().rows().len(), case.row_count, "{id}");
        let actual = goal_projection(&restored);
        let ts = object(&case.entry["ts"], id);
        assert_eq!(
            keys(ts),
            BTreeSet::from(["current", "failure", "seenGoalIds"]),
            "{id}"
        );
        match case.entry.get("rust") {
            Some(rust) => {
                let rust = object(rust, id);
                assert_eq!(
                    keys(rust),
                    BTreeSet::from(["limit", "outcome", "seq"]),
                    "{id}"
                );
                assert_eq!(rust["outcome"], "native-subset", "{id}");
                let name = text(&rust["limit"], id);
                let (_, limit) = LIMITS
                    .iter()
                    .find(|(known, _)| *known == name)
                    .unwrap_or_else(|| panic!("{id}: unknown limit {name}"));
                let seq = rust["seq"].as_u64().expect("limit seq");
                assert_eq!(actual, Err(GoalRefusal { seq, limit: *limit }), "{id}");
                limits.insert(name);
            }
            None => {
                let state = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                assert_eq!(state_value(&state), Value::Object(ts.clone()), "{id}");
            }
        }
    }
    let all: BTreeSet<&str> = LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(limits, all, "every limit is witnessed");
}
