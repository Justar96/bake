//! Runs every shared case in `conformance/session/inbox-cases.json` through
//! `restore_plain_log`, then `restored_inbox` and `consumed_work`, over the
//! same bytes the TypeScript spec builds. Each case restores; its expected
//! outcomes are its `rust` overrides when present, otherwise the
//! hand-written TypeScript outcomes. Nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    ConsumedWorkCoercion, ConsumedWorkLimit, InboxLimit, InboxRefusal, PathPlatform, RestoredLog,
    consumed_work, restore_plain_log, restored_inbox,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/inbox-cases";
const ORACLE: &str = "inboxProjectionDefinition.apply from init() and foldConsumedWork over the events restorePlainLog(log) restores in packages/core/agent-loop/tests/inbox-conformance.spec.ts";
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
const CASE_COUNT: usize = 63;
const SOURCE_BUDGET: usize = 64;
const INBOX_LIMITS: [(&str, InboxLimit); 4] = [
    ("target", InboxLimit::Target),
    ("count", InboxLimit::Count),
    ("inserted", InboxLimit::Inserted),
    ("message-id", InboxLimit::MessageId),
];
const CONSUMED_WORK_LIMITS: [(&str, ConsumedWorkCoercion); 4] = [
    ("data", ConsumedWorkCoercion::Data),
    ("turn", ConsumedWorkCoercion::Turn),
    ("inserted", ConsumedWorkCoercion::Inserted),
    ("reason", ConsumedWorkCoercion::Reason),
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
    /// The edited rows as parsed, for `$log` references.
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
    let mut tail = String::new();
    let edits = entry["edits"].as_array().expect("edits");
    for edit in edits {
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
            ["tail"] => tail = line("tail"),
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
    log.push_str(&tail);
    (log.into_bytes(), rows)
}

fn load() -> Vec<Case> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/inbox-cases.json")).expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "logs", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    let logs = object(&table["logs"], "logs");
    assert_eq!(logs.len(), LOGS.len());
    for (name, path, _) in LOGS {
        assert_eq!(logs[name], path);
    }
    let cases = table["cases"].as_array().expect("cases");
    cases
        .iter()
        .map(|entry| {
            let entry = object(entry, "case").clone();
            let id = text(&entry["id"], "case id").to_owned();
            let allowed = BTreeSet::from(["id", "log", "edits", "ts", "rust", "note"]);
            assert!(keys(&entry).is_subset(&allowed), "{id}: unknown keys");
            let (log, rows) = build(&entry, &id);
            let rows = rows
                .iter()
                .map(|row| serde_json::from_str(row).unwrap_or(Value::Null))
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
fn resolve(value: &Value, case: &Case, closers: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve(item, case, closers))
                .collect(),
        ),
        Value::Object(fields) => {
            if fields.keys().any(|key| key.starts_with('$')) {
                assert_eq!(fields.len(), 1, "{}: invalid reference", case.id);
                let (key, pointer) = fields.iter().next().expect("one member");
                let pointer = text(pointer, &case.id);
                return match key.as_str() {
                    "$log" => at(&Value::Array(case.rows.clone()), pointer, &case.id).clone(),
                    "$closer" => at(closers, pointer, &case.id).clone(),
                    _ => panic!("{}: invalid reference {key}", case.id),
                };
            }
            Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| (key.clone(), resolve(item, case, closers)))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

/// The override for one fold, as a limit name and seq.
fn limit_override<'a>(case: &'a Case, fold: &str) -> Option<(&'a str, u64)> {
    let rust = object(case.entry.get("rust")?, &case.id).get(fold)?;
    let rust = object(rust, &case.id);
    assert_eq!(
        keys(rust),
        BTreeSet::from(["limit", "outcome", "seq"]),
        "{}",
        case.id
    );
    assert_eq!(rust["outcome"], "native-subset", "{}", case.id);
    let seq = rust["seq"].as_u64().expect("limit seq");
    Some((text(&rust["limit"], &case.id), seq))
}

fn check_inbox(case: &Case, restored: &RestoredLog, closers: &Value) -> Option<&'static str> {
    let id = &case.id;
    let actual = restored_inbox(restored);
    if let Some((name, seq)) = limit_override(case, "inbox") {
        let (known, expected) = INBOX_LIMITS
            .iter()
            .find(|(known, _)| *known == name)
            .unwrap_or_else(|| panic!("{id}: unknown inbox limit {name}"));
        assert_eq!(
            actual,
            Err(InboxRefusal::NativeSubset {
                seq,
                limit: *expected
            }),
            "{id}"
        );
        return Some(known);
    }
    let expected = resolve(&case.entry["ts"]["inbox"], case, closers);
    let actual = match actual {
        Ok(inbox) => json!({
            "outcome": "pending",
            "next-turn": inbox.next_turn,
            "next-step": inbox.next_step,
        }),
        Err(refusal) => json!({
            "outcome": "rejected",
            "message": refusal.message().unwrap_or_else(|| panic!("{id}: {refusal:?}")),
        }),
    };
    assert_eq!(actual, expected, "{id}: inbox");
    None
}

fn check_consumed_work(
    case: &Case,
    restored: &RestoredLog,
    closers: &Value,
) -> Option<&'static str> {
    let id = &case.id;
    let actual = consumed_work(restored);
    if let Some((name, seq)) = limit_override(case, "consumedWork") {
        let (known, cause) = CONSUMED_WORK_LIMITS
            .iter()
            .find(|(known, _)| *known == name)
            .unwrap_or_else(|| panic!("{id}: unknown consumed-work limit {name}"));
        assert_eq!(
            actual,
            Err(ConsumedWorkLimit { seq, cause: *cause }),
            "{id}"
        );
        return Some(known);
    }
    let expected = resolve(&case.entry["ts"]["consumedWork"], case, closers);
    assert_eq!(
        expected["outcome"], "folded",
        "{id}: a TypeError names its Rust limit"
    );
    let work = actual.unwrap_or_else(|limit| panic!("{id}: {limit:?}"));
    let mut actual = json!({"outcome": "folded"});
    if let Some(end) = work.end {
        actual["end"] = end;
    }
    actual["droppedUnrun"] = work.dropped_unrun.into();
    assert_eq!(actual, expected, "{id}: consumed work");
    None
}

#[test]
fn shared_cases_fold_like_the_typescript_projections() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut inbox_limits = BTreeSet::new();
    let mut work_limits = BTreeSet::new();
    let mut refusals = 0;
    for case in &cases {
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{}: every case restores: {refusal:?}", case.id));
        let closers = Value::Array(restored.closers().to_vec());
        inbox_limits.extend(check_inbox(case, &restored, &closers));
        work_limits.extend(check_consumed_work(case, &restored, &closers));
        refusals += usize::from(case.entry["ts"]["inbox"]["outcome"] == "rejected");
    }
    let all: BTreeSet<&str> = INBOX_LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(inbox_limits, all, "every inbox limit is witnessed");
    let all: BTreeSet<&str> = CONSUMED_WORK_LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(work_limits, all, "every consumed-work limit is witnessed");
    assert!(refusals > 0, "an inbox refusal is witnessed");
}
