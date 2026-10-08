//! Runs every shared case in `conformance/session/prefix-restore-cases.json`
//! through `restore_plain_log`. Each case is one row prefix of a runtime
//! capture, from the header alone to the whole log, and its expected state
//! was written by hand from the TypeScript sources; the TypeScript spec
//! checks the same table with `restorePlainLog`. Each prefix followed by its
//! next row torn after 1 byte, half its length, and all but its last byte
//! must restore exactly as the prefix does, with the prefix's committed bytes
//! and that offset as its truncation point. Messages are also compared as
//! serialized text, since `Value` equality ignores member order. Nothing
//! here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    HeaderOrigin, PathPlatform, RestoredLog, SessionHeader, TornTail, restore_plain_log,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/prefix-restore-cases";
const ORACLE: &str = "restorePlainLog(prefix) in packages/session/session-persistence-jsonl/tests/prefix-restore-conformance.spec.ts: scanLog, validateStoredEvents, interruptedTurnClosers, then Session.fromRestore(..., \"detached\", currentSessionMessageProjections)";
/// Each capture, its size, and its committed row count after the header; the
/// TypeScript spec checks their SHA-256.
const LOGS: [(&str, &str, usize, usize); 3] = [
    (
        "tool-call-turn",
        "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl",
        4533,
        16,
    ),
    (
        "dynamic-tools",
        "conformance/runtime/request-reconstruction/dynamic-tools/session.jsonl",
        9755,
        38,
    ),
    (
        "retry-attempt",
        "conformance/runtime/request-reconstruction/retry-attempt/session.jsonl",
        3102,
        14,
    ),
];
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 71;
/// Torn variants per prefix with a following row.
const TORN_COUNT: usize = 3 * (16 + 38 + 14);
const SOURCE_BUDGET: usize = 64;
const EXPECTED_KEYS: [&str; 7] = [
    "closers",
    "endSeedAppended",
    "messages",
    "requestContext",
    "requestHeader",
    "storedEventCount",
    "toolHistory",
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

fn count(value: &Value, context: &str) -> usize {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| panic!("{context}: expected a count"))
}

struct Capture {
    header: String,
    rows: Vec<String>,
    expected_header: Value,
    inherited_event_count: usize,
}

struct Case {
    id: String,
    log: String,
    rows: usize,
    ts: Value,
}

fn read_capture(name: &str, entry: &Value) -> Capture {
    let (_, path, size, rows) = LOGS
        .iter()
        .find(|(log, ..)| *log == name)
        .unwrap_or_else(|| panic!("unknown log {name}"));
    let entry = object(entry, name);
    assert_eq!(
        keys(entry),
        BTreeSet::from(["header", "inheritedEventCount", "path"]),
        "{name}"
    );
    assert_eq!(entry["path"], *path, "{name}");
    let source = std::fs::read_to_string(repo_path(path)).expect("read capture");
    assert_eq!(source.len(), *size, "{path} changed");
    let mut lines: Vec<String> = source
        .strip_suffix('\n')
        .expect("final LF")
        .split('\n')
        .map(str::to_owned)
        .collect();
    let header = lines.remove(0);
    assert_eq!(lines.len(), *rows, "{path} changed its row count");
    Capture {
        header,
        rows: lines,
        expected_header: Value::Object(object(&entry["header"], name).clone()),
        inherited_event_count: count(&entry["inheritedEventCount"], name),
    }
}

fn load() -> (Vec<(String, Capture)>, Vec<Case>) {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/prefix-restore-cases.json"))
            .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "logs", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    assert!(
        table["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    let logs = object(&table["logs"], "logs");
    assert_eq!(logs.len(), LOGS.len());
    let captures: Vec<(String, Capture)> = LOGS
        .iter()
        .map(|(name, ..)| ((*name).to_owned(), read_capture(name, &logs[*name])))
        .collect();
    let cases = table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|entry| {
            let entry = object(entry, "case");
            let id = text(&entry["id"], "case id").to_owned();
            assert_eq!(
                keys(entry),
                BTreeSet::from(["id", "log", "rows", "ts"]),
                "{id}"
            );
            let log = text(&entry["log"], &id).to_owned();
            let rows = count(&entry["rows"], &id);
            let capture = &captures
                .iter()
                .find(|(name, _)| *name == log)
                .unwrap_or_else(|| panic!("{id}: unknown log {log}"))
                .1;
            assert!(rows <= capture.rows.len(), "{id}: rows past the end");
            assert_eq!(id, format!("{log}/{rows}"), "{id}: id names its prefix");
            let ts = entry["ts"].clone();
            assert_eq!(
                keys(object(&ts, &id)),
                BTreeSet::from(EXPECTED_KEYS),
                "{id}"
            );
            assert_eq!(count(&ts["storedEventCount"], &id), rows, "{id}");
            Case { id, log, rows, ts }
        })
        .collect();
    (captures, cases)
}

/// The header and the first `rows` committed rows, each with its LF.
fn prefix(capture: &Capture, rows: usize) -> String {
    let mut log = String::new();
    for line in std::iter::once(&capture.header).chain(&capture.rows[..rows]) {
        log.push_str(line);
        log.push('\n');
    }
    log
}

/// Resolve a JSON pointer without `~` escapes, refusing a missing member.
fn at<'a>(root: &'a Value, pointer: &str, id: &str) -> &'a Value {
    assert!(
        pointer.starts_with('/') && !pointer.contains('~'),
        "{id}: unsupported pointer {pointer}"
    );
    pointer.split('/').skip(1).fold(root, |node, key| {
        let next = match node {
            Value::Array(items) => key.parse::<usize>().ok().and_then(|index| items.get(index)),
            Value::Object(fields) => fields.get(key),
            _ => None,
        };
        next.unwrap_or_else(|| panic!("{id}: {pointer} does not exist"))
    })
}

/// Replace each `{ "$log": pointer }` and `{ "$closer": pointer }` with the
/// value it names.
fn resolve(value: &Value, id: &str, rows: &Value, closers: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve(item, id, rows, closers))
                .collect(),
        ),
        Value::Object(fields) => {
            if fields.keys().any(|key| key.starts_with('$')) {
                assert_eq!(fields.len(), 1, "{id}: invalid reference");
                let (key, pointer) = fields.iter().next().expect("one member");
                let pointer = text(pointer, id);
                return match key.as_str() {
                    "$log" => at(rows, pointer, id).clone(),
                    "$closer" => at(closers, pointer, id).clone(),
                    _ => panic!("{id}: invalid reference {key}"),
                };
            }
            Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| (key.clone(), resolve(item, id, rows, closers)))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

/// The independent expectation for one prefix, with references resolved
/// against that prefix's rows.
fn expected(case: &Case, capture: &Capture) -> Value {
    let rows = Value::Array(
        capture.rows[..case.rows]
            .iter()
            .map(|row| serde_json::from_str(row).expect("capture row"))
            .collect(),
    );
    let closers = resolve(&case.ts["closers"], &case.id, &rows, &Value::Null);
    let mut want = resolve(&case.ts, &case.id, &rows, &closers);
    let fields = want.as_object_mut().expect("expectation object");
    fields.insert("header".into(), capture.expected_header.clone());
    fields.insert(
        "inheritedEventCount".into(),
        capture.inherited_event_count.into(),
    );
    fields.insert(
        "committedBytes".into(),
        prefix(capture, case.rows).len().into(),
    );
    want
}

fn header_meta(header: &SessionHeader) -> Value {
    let mut meta = json!({
        "version": 3,
        "id": header.id,
        "createdAt": header.created_at,
        "isSeeded": header.is_seeded,
        "delegationDepth": header.delegation_depth,
    });
    let fields = meta.as_object_mut().expect("meta object");
    if let Some(cwd) = &header.cwd {
        fields.insert("cwd".into(), json!(cwd));
    }
    if let Some(parent) = &header.parent_session {
        fields.insert("parentSession".into(), json!(parent));
    }
    if let Some(HeaderOrigin::Subagent) = header.origin {
        fields.insert("origin".into(), json!("subagent"));
    }
    if let Some(preset) = &header.agent_preset {
        fields.insert("agentPreset".into(), json!(preset));
    }
    meta
}

/// The restored state in the table's form.
fn restored_value(restored: &RestoredLog) -> Value {
    let stored = restored.stored();
    json!({
        "header": header_meta(stored.header()),
        "inheritedEventCount": stored.inherited_event_count(),
        "committedBytes": stored.committed_bytes(),
        "storedEventCount": stored.rows().len(),
        "closers": restored.closers(),
        "endSeedAppended": restored.end_seed_appended(),
        "messages": restored.messages(),
        "requestHeader": restored.request_header(),
        "toolHistory": restored.tool_history(),
        "requestContext": restored.request_context(),
    })
}

/// The byte lengths a following row is torn at.
const fn torn_lengths(row: &str) -> [usize; 3] {
    let length = row.len();
    [1, length / 2, length.saturating_sub(1)]
}

#[test]
fn every_row_prefix_restores_like_the_read_path() {
    let (captures, cases) = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    for (name, capture) in &captures {
        let swept: Vec<usize> = cases
            .iter()
            .filter(|case| case.log == *name)
            .map(|case| case.rows)
            .collect();
        let all: Vec<usize> = (0..=capture.rows.len()).collect();
        assert_eq!(swept, all, "{name}: every prefix once, in order");
    }
    let mut torn_variants = 0;
    for case in &cases {
        let id = &case.id;
        let capture = &captures
            .iter()
            .find(|(name, _)| *name == case.log)
            .expect("capture")
            .1;
        let want = expected(case, capture);
        let log = prefix(capture, case.rows);
        let restored = restore_plain_log(log.as_bytes(), PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
        let actual = restored_value(&restored);
        assert_eq!(actual, want, "{id}");
        assert_eq!(
            actual["messages"].to_string(),
            want["messages"].to_string(),
            "{id}: member order"
        );
        assert_eq!(
            restored.torn(),
            None,
            "{id}: a whole prefix has no torn tail"
        );
        // Restoration returns the scanned rows unchanged.
        for (row, text) in restored.stored().rows().iter().zip(&capture.rows) {
            assert_eq!(
                *row,
                serde_json::from_str::<Value>(text).expect("row"),
                "{id}"
            );
        }
        let Some(next) = capture.rows.get(case.rows) else {
            continue;
        };
        // A writer stopped inside the next row: its partial bytes are no record.
        for length in torn_lengths(next) {
            let mut torn = log.clone().into_bytes();
            torn.extend_from_slice(&next.as_bytes()[..length]);
            let restored = restore_plain_log(&torn, PathPlatform::Posix, SOURCE_BUDGET)
                .unwrap_or_else(|refusal| panic!("{id} torn after {length}: {refusal:?}"));
            let actual = restored_value(&restored);
            assert_eq!(actual, want, "{id} torn after {length}");
            assert_eq!(
                actual["messages"].to_string(),
                want["messages"].to_string(),
                "{id} torn after {length}: member order"
            );
            assert_eq!(
                restored.torn(),
                Some(TornTail {
                    truncate_to: log.len(),
                    recovered_from: case.rows,
                }),
                "{id} torn after {length}"
            );
            torn_variants += 1;
        }
    }
    assert_eq!(torn_variants, TORN_COUNT);
}
