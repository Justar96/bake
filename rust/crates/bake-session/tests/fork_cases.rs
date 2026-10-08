//! Runs every shared case in `conformance/session/fork-cases.json` through
//! `restore_plain_log` and then `fork_seed`, over the same bytes the
//! TypeScript spec builds. A case's expected outcome is its `rust` override
//! when present, otherwise the hand-written inherited counts or refusal the
//! TypeScript `SessionStore.fork` also meets. Nothing here reads TypeScript
//! output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{ForkLimit, ForkRefusal, PathPlatform, fork_seed, restore_plain_log};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/fork-cases";
const ORACLE: &str = "SessionStore.fork(source, boundary) in packages/core/session/src/index.ts over a source restored as SessionStore.prepare does: the decoded rows and interruptedTurnClosers passed to Session.fromRestore(..., \"detached\") with no message projections";
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
const CASE_COUNT: usize = 48;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [(&str, ForkLimit); 1] = [("turn-diagnostic", ForkLimit::TurnDiagnostic)];
/// Every TypeScript refusal a case must witness: class and code.
const REFUSALS: [(&str, Option<&str>); 3] = [
    ("SessionForkError", Some("INVALID_BOUNDARY")),
    ("SessionForkError", Some("OPEN_TURN")),
    ("Error", None),
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
    /// The edited rows as parsed.
    rows: Vec<Value>,
    entry: Map<String, Value>,
}

/// Apply a case's text edits to its capture, as the TypeScript spec does.
fn build(entry: &Map<String, Value>, id: &str) -> (Vec<u8>, Vec<String>) {
    let name = text(&entry["log"], id);
    let (_, path, size) = LOGS
        .iter()
        .find(|(log, _, _)| *log == name)
        .unwrap_or_else(|| panic!("{id}: unknown log {name}"));
    let source = std::fs::read_to_string(repo_path(path)).expect("read capture");
    assert_eq!(source.len(), *size, "{path} changed");
    let mut lines: Vec<String> = source
        .strip_suffix('\n')
        .expect("final LF")
        .split('\n')
        .map(str::to_owned)
        .collect();
    let mut header = lines.remove(0);
    let mut rows = lines;
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
    (log.into_bytes(), rows)
}

fn load() -> Vec<Case> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/fork-cases.json")).expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "logs", "oracle", "provenance", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    assert!(table["provenance"].is_string());
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
            let allowed = BTreeSet::from(["id", "log", "edits", "boundary", "ts", "rust", "note"]);
            assert!(keys(&entry).is_subset(&allowed), "{id}: unknown keys");
            let (log, rows) = build(&entry, &id);
            let rows = rows
                .iter()
                .map(|row| serde_json::from_str(row).expect("rows parse"))
                .collect();
            Case {
                id,
                log,
                rows,
                entry,
            }
        })
        .collect()
}

/// A stored row as `scanLog` decodes it: packed `[start, end]` source ranges
/// expanded, independently of the crate's decoder.
fn decoded(row: &Value) -> Value {
    let mut row = row.clone();
    if let Some(Value::Array(entries)) = row.get("sourceEventSeqs") {
        let expanded: Vec<Value> = entries
            .iter()
            .flat_map(|entry| match entry {
                Value::Array(range) => {
                    let start = range[0].as_u64().expect("range start");
                    let end = range[1].as_u64().expect("range end");
                    (start..=end).map(Value::from).collect()
                }
                seq => vec![seq.clone()],
            })
            .collect();
        row["sourceEventSeqs"] = Value::Array(expanded);
    }
    row
}

#[test]
fn shared_cases_fork_like_the_session_store() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut limits = BTreeSet::new();
    let mut refusals = BTreeSet::new();
    let mut unrepresentable = 0;
    let mut appended = 0;
    for case in &cases {
        let id = &case.id;
        let ts = object(&case.entry["ts"], id);
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: every source restores: {refusal:?}"));
        let boundary = case.entry.get("boundary");
        if let Some(rust) = case.entry.get("rust") {
            let rust = object(rust, id);
            match text(&rust["outcome"], id) {
                "unrepresentable" => {
                    let boundary = boundary.expect("an unrepresentable boundary is given");
                    assert!(boundary.is_number(), "{id}");
                    assert_eq!(boundary.as_u64(), None, "{id}: u64 boundary");
                    unrepresentable += 1;
                }
                "native-subset" => {
                    let name = text(&rust["limit"], id);
                    let (_, expected) = LIMITS
                        .iter()
                        .find(|(known, _)| *known == name)
                        .unwrap_or_else(|| panic!("{id}: unknown limit {name}"));
                    let actual = fork_seed(&restored, boundary.map(|b| b.as_u64().expect("u64")));
                    let Err(ForkRefusal::NativeSubset { seq, limit }) = actual else {
                        panic!("{id}: expected a native limit, got {actual:?}");
                    };
                    assert_eq!(limit, *expected, "{id}");
                    // The limit names the open turn's own row.
                    let row = &case.rows[usize::try_from(seq).expect("row index")];
                    assert_eq!(row["type"], "turn/start", "{id}");
                    limits.insert(name);
                }
                other => panic!("{id}: invalid rust outcome {other}"),
            }
            continue;
        }
        let boundary = boundary.map(|boundary| boundary.as_u64().expect("u64 boundary"));
        let actual = fork_seed(&restored, boundary);
        match text(&ts["outcome"], id) {
            "forked" => {
                let seed = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                let stored = index(&ts["stored"], id);
                let closers = index(&ts["closers"], id);
                let end_seed = ts["endSeed"].as_bool().expect("endSeed");
                assert_eq!(seed.events().len(), stored + closers, "{id}: events");
                assert_eq!(seed.end_seed(), end_seed, "{id}: end seed");
                assert_eq!(
                    seed.inherited_event_count(),
                    (stored + closers + usize::from(end_seed)) as u64,
                    "{id}"
                );
                if closers > 0 || end_seed {
                    assert_eq!(stored, case.rows.len(), "{id}: every stored row");
                }
                if end_seed {
                    assert!(restored.end_seed_appended(), "{id}");
                    assert_eq!(closers, restored.closers().len(), "{id}: every closer");
                    appended += 1;
                }
                let expected: Vec<Value> = case.rows[..stored]
                    .iter()
                    .map(decoded)
                    .chain(restored.closers()[..closers].iter().cloned())
                    .collect();
                assert_eq!(seed.events(), expected.as_slice(), "{id}: inherited events");
                for (seq, event) in (0u64..).zip(seed.events()) {
                    assert_eq!(event["seq"], seq, "{id}: contiguous");
                }
            }
            "rejected" => {
                let refusal = actual.expect_err(id);
                let class = text(&ts["class"], id);
                let code = ts.get("code").map(|code| text(code, id));
                assert_eq!(refusal.class(), Some(class), "{id}: class");
                assert_eq!(refusal.code(), code, "{id}: code");
                assert_eq!(
                    refusal.message().as_deref(),
                    Some(text(&ts["message"], id)),
                    "{id}: message"
                );
                refusals.insert((class, code));
            }
            other => panic!("{id}: invalid outcome {other}"),
        }
    }
    let all: BTreeSet<&str> = LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(limits, all, "every limit is witnessed");
    assert_eq!(
        refusals,
        REFUSALS.into_iter().collect(),
        "every refusal is witnessed"
    );
    assert!(
        unrepresentable > 0,
        "an unrepresentable boundary is witnessed"
    );
    assert!(appended > 0, "an inherited appended end seed is witnessed");
}
