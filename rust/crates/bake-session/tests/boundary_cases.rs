//! Runs every shared case in `conformance/session/boundary-cases.json`
//! through `restore_plain_log` and then `turn_boundary` or `session_title`,
//! over the same bytes the TypeScript spec folds. Every case must restore. A
//! case's expected outcome is its `rust` override when present, a native
//! limit at the seq Rust refuses, otherwise the hand-written value the
//! TypeScript fold also meets. Nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    BoundaryLimit, BoundaryRefusal, PathPlatform, TurnBoundaryState, restore_plain_log,
    session_title, turn_boundary,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/boundary-cases";
const ORACLE: &str = "turnBoundaryProjectionDefinition in packages/core/agent-loop/src/index.ts and titleProjectionDefinition in packages/session/session-title/src/index.ts, init and apply folded over each case's parsed rows and their interruptedTurnClosers";
/// The captures and their sizes; the TypeScript spec checks their SHA-256.
const LOGS: [(&str, &str, usize); 2] = [
    (
        "tool-call-turn",
        "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl",
        4533,
    ),
    (
        "dynamic-tools",
        "conformance/runtime/request-reconstruction/dynamic-tools/session.jsonl",
        9755,
    ),
];
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 52;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [(&str, BoundaryLimit); 3] = [
    ("undefined-member", BoundaryLimit::UndefinedMember),
    ("null-data", BoundaryLimit::NullData),
    ("number", BoundaryLimit::Number),
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

/// Apply a case's edits to its capture, as the TypeScript spec does.
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
    let mut header = rows.remove(0);
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
            ["header"] => header = line("header"),
            ["append"] => rows.push(line("append")),
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
        &std::fs::read(repo_path("conformance/session/boundary-cases.json")).expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "logs", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
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
            let allowed = BTreeSet::from(["id", "fold", "log", "edits", "ts", "rust", "note"]);
            assert!(keys(&entry).is_subset(&allowed), "{id}: unknown keys");
            assert!(
                entry.get("note").is_none_or(Value::is_string),
                "{id}: invalid note"
            );
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

/// The folded state in the table's form, `TurnBoundaryProjection` on the wire.
fn boundary_value(state: &TurnBoundaryState) -> Value {
    json!({
        "openTurnStartSeq": state.open_turn_start_seq,
        "lastStepStartSeq": state.last_step_start_seq,
        "lastStepBoundary": state.last_step_boundary.map(|boundary| json!({
            "kind": boundary.kind.as_str(),
            "seq": boundary.seq,
        })),
        "lastTurn": state.last_turn,
    })
}

/// The limit a `ts` value requires: an absent `lastTurn` or `title` is
/// JavaScript's `undefined`, and `threw` a `TypeError`.
fn required_limit(fold: &str, ts: &Map<String, Value>, id: &str) -> Option<&'static str> {
    let fields = keys(ts);
    match fold {
        "turnBoundary" => {
            let base = BTreeSet::from(["lastStepBoundary", "lastStepStartSeq", "openTurnStartSeq"]);
            let mut full = base.clone();
            full.insert("lastTurn");
            if fields == full {
                None
            } else {
                assert_eq!(fields, base, "{id}: invalid turnBoundary state");
                Some("undefined-member")
            }
        }
        "title" => {
            if fields == BTreeSet::from(["title"]) {
                None
            } else if fields.is_empty() {
                Some("undefined-member")
            } else {
                assert_eq!(ts.get("threw"), Some(&json!("TypeError")), "{id}");
                assert_eq!(fields, BTreeSet::from(["threw"]), "{id}");
                Some("null-data")
            }
        }
        other => panic!("{id}: unknown fold {other}"),
    }
}

#[test]
fn shared_cases_fold_like_the_boundary_and_title_projections() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut limits = BTreeSet::new();
    let mut folds = BTreeSet::new();
    for case in &cases {
        let id = &case.id;
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: every case restores, got {refusal:?}"));
        // Both arms fold the same rows: no torn tail is left out.
        assert_eq!(restored.stored().rows().len(), case.row_count, "{id}");
        let fold = text(&case.entry["fold"], id);
        folds.insert(fold.to_owned());
        let ts = object(&case.entry["ts"], id);
        let required = required_limit(fold, ts, id);
        let actual = match fold {
            "turnBoundary" => turn_boundary(&restored).map(|state| boundary_value(&state)),
            _ => session_title(&restored).map(|title| json!({"title": title})),
        };
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
                if let Some(required) = required {
                    assert_eq!(name, required, "{id}: ts requires this limit");
                }
                let (_, limit) = LIMITS
                    .iter()
                    .find(|(known, _)| *known == name)
                    .unwrap_or_else(|| panic!("{id}: unknown limit {name}"));
                let seq = rust["seq"].as_u64().expect("limit seq");
                assert_eq!(actual, Err(BoundaryRefusal { seq, limit: *limit }), "{id}");
                limits.insert(name);
            }
            None => {
                assert_eq!(required, None, "{id}: ts needs a rust limit");
                let value = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                assert_eq!(value, Value::Object(ts.clone()), "{id}");
            }
        }
    }
    let all: BTreeSet<&str> = LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(limits, all, "every limit is witnessed");
    assert_eq!(
        folds,
        BTreeSet::from(["title".to_owned(), "turnBoundary".to_owned()])
    );
}
