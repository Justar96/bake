//! Runs every shared case in `conformance/session/pressure-cases.json`
//! through `restore_plain_log` and then `context_pressure`, over the same
//! bytes the TypeScript spec folds. Every case must restore. A case's
//! expected outcome is its `rust` override when present, a native limit at
//! the seq where Rust refuses, otherwise the hand-written state and view, or
//! the replacement error, the TypeScript fold also meets. Nothing here reads
//! TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    ContextPressureState, PathPlatform, PressureLimit, PressureRefusal, RequestRoute,
    context_pressure, restore_plain_log,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/pressure-cases";
const ORACLE: &str = "contextPressureProjectionDefinition.init/apply and wire.view in packages/llm/token-meter/src/usage-projection.ts, folded over the events Session.fromRestore holds for each case's parsed rows and their interruptedTurnClosers";
/// Each capture and its size; the TypeScript spec checks their SHA-256.
const LOGS: [(&str, &str, usize); 3] = [
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
    (
        "retry-attempt",
        "conformance/runtime/request-reconstruction/retry-attempt/session.jsonl",
        3102,
    ),
];
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 37;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [(&str, PressureLimit); 7] = [
    ("number", PressureLimit::Number),
    ("usage", PressureLimit::Usage),
    ("stream", PressureLimit::Stream),
    ("claim", PressureLimit::Claim),
    ("block", PressureLimit::Block),
    ("route", PressureLimit::Route),
    ("context-window", PressureLimit::ContextWindow),
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
            ["find", "replace", "row"] => {
                let row = index(&edit["row"], id);
                let find = line("find");
                assert!(!find.is_empty(), "{id}: empty find");
                assert_eq!(rows[row].matches(&find).count(), 1, "{id}: find once");
                rows[row] = rows[row].replacen(&find, &line("replace"), 1);
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
        &std::fs::read(repo_path("conformance/session/pressure-cases.json")).expect("read table"),
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

fn route_value(route: &RequestRoute) -> Value {
    json!({"provider": route.provider, "model": route.model})
}

/// Insert `value` under `key` only when present, as the TypeScript fold
/// omits an undefined member.
fn optional(fields: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        fields.insert(key.to_owned(), value);
    }
}

/// The folded state and its view in the table's form.
fn folded_value(state: &ContextPressureState) -> Value {
    let mut fields = Map::new();
    optional(
        &mut fields,
        "contextWindow",
        state.context_window.map(Value::from),
    );
    optional(
        &mut fields,
        "sampledContextWindow",
        state.sampled_context_window.map(Value::from),
    );
    optional(
        &mut fields,
        "pressureTokens",
        state.pressure_tokens.map(Value::from),
    );
    optional(
        &mut fields,
        "requestRoute",
        state.request_route.as_ref().map(route_value),
    );
    optional(
        &mut fields,
        "sampledRoute",
        state.sampled_route.as_ref().map(route_value),
    );
    fields.insert("surfaceTokens".to_owned(), state.surface_tokens.into());
    optional(
        &mut fields,
        "sampledSurfaceTokens",
        state.sampled_surface_tokens.map(Value::from),
    );
    let view = state.view();
    let mut wire = Map::new();
    optional(
        &mut wire,
        "contextWindow",
        view.context_window.map(Value::from),
    );
    optional(
        &mut wire,
        "sampledContextWindow",
        view.sampled_context_window.map(Value::from),
    );
    optional(
        &mut wire,
        "requestRoute",
        view.request_route.as_ref().map(route_value),
    );
    optional(
        &mut wire,
        "sampledRoute",
        view.sampled_route.as_ref().map(route_value),
    );
    optional(
        &mut wire,
        "pressureTokens",
        view.pressure_tokens.map(Value::from),
    );
    optional(
        &mut wire,
        "projectedTokens",
        view.projected_tokens.map(Value::from),
    );
    json!({"outcome": "folded", "state": fields, "view": wire})
}

#[test]
fn shared_cases_fold_like_the_context_pressure_projection() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut limits = BTreeSet::new();
    let mut errors = 0;
    for case in &cases {
        let id = &case.id;
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: every case restores, got {refusal:?}"));
        // Both arms fold the same rows: no torn tail is left out.
        assert_eq!(restored.stored().rows().len(), case.row_count, "{id}");
        let actual = context_pressure(&restored);
        let ts = object(&case.entry["ts"], id);
        if let Some(rust) = case.entry.get("rust") {
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
            if ts["outcome"] == "rejected" {
                assert_eq!(
                    ts["class"], "TypeError",
                    "{id}: only a TypeError is limited"
                );
                assert_eq!(ts["seq"], seq, "{id}: the row TypeScript throws at");
            }
            assert_eq!(
                actual,
                Err(PressureRefusal::NativeSubset { seq, limit: *limit }),
                "{id}"
            );
            limits.insert(name);
            continue;
        }
        match text(&ts["outcome"], id) {
            "folded" => {
                let state = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                assert_eq!(folded_value(&state), Value::Object(ts.clone()), "{id}");
            }
            "rejected" => {
                assert_eq!(
                    keys(ts),
                    BTreeSet::from(["class", "message", "outcome", "seq"]),
                    "{id}: a TypeError names its Rust limit"
                );
                assert_eq!(ts["class"], "Error", "{id}");
                let refusal = actual.expect_err(id);
                let PressureRefusal::UnclaimedReplace { seq, .. } = refusal else {
                    panic!("{id}: {refusal:?}");
                };
                assert_eq!(ts["seq"], seq, "{id}");
                assert_eq!(refusal.message().as_deref(), ts["message"].as_str(), "{id}");
                errors += 1;
            }
            other => panic!("{id}: unknown outcome {other}"),
        }
    }
    assert!(errors > 0, "the replacement error is witnessed");
    let all: BTreeSet<&str> = LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(limits, all, "every limit is witnessed");
}

/// `view` is total over hand-built states: a sum past `i64` saturates and a
/// negative one floors at 0, as the definition's `Math.max(0, …)` does.
#[test]
fn hand_built_view_saturates() {
    let huge = ContextPressureState {
        pressure_tokens: Some(i64::MAX),
        sampled_surface_tokens: Some(0),
        surface_tokens: 1,
        ..Default::default()
    };
    assert_eq!(huge.view().projected_tokens, Some(i64::MAX));
    let negative = ContextPressureState {
        pressure_tokens: Some(i64::MIN),
        sampled_surface_tokens: Some(i64::MAX),
        surface_tokens: -1,
        ..Default::default()
    };
    assert_eq!(negative.view().projected_tokens, Some(0));
}
