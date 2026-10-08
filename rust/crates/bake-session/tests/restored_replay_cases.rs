//! Runs every shared case in
//! `conformance/runtime/restored-request-derivation-cases.json` through
//! `replay_restored_requests`. A plain case edits a runtime capture as the
//! TypeScript spec does and restores it with `restore_plain_log`; a migrated
//! case migrates its released header and rows with `migrate_v2_rows`, or
//! `decode_v0_v1_items` and `migrate_released_history`, under both path
//! platforms, and restores the result with `restore_migrated`. Requests are
//! compared with the hand-written expectation, `$log` references resolved
//! against the case's rows, and their messages also as serialized text,
//! since `Value` equality ignores member order. Rust also checks that
//! `replay_requests` refuses every seeded plain case. Nothing here reads
//! TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    MigratedRestoreLimit, MigratedRestoreRefusal, MigratedV2, PathPlatform, ReplayRefusal,
    RestoreLimit, RestoreRefusal, RestoredLog, RestoredReplayLimit, RestoredReplayRefusal,
    V1CodecRecovery, V1CodecVersion, decode_v0_v1_items, migrate_released_history, migrate_v2_rows,
    replay_requests, replay_restored_requests, restore_migrated, restore_plain_log,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/runtime-conformance/restored-request-derivation-cases";
const ORACLE: &str = "deriveRestoredRequests in packages/core/agent-loop/tests/restored-request-derivation-conformance.spec.ts: the restored events of restorePlainLog (scanLog, validateStoredEvents, interruptedTurnClosers, Session.fromRestore) or of readColdSessionLog for a migrated file, then for each settlement at or after the inherited cut, Session.fromRestore(prefix, ..., \"detached\", currentSessionMessageProjections) and foldRequestHeader(prefix) assembled as replayRequests assembles them";
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
const CASE_COUNT: usize = 32;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 3] = ["coordinate", "repeated-coordinate", "restore/number"];
const CAUSES: [&str; 2] = ["no-later-settlement", "no-request-header"];

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

fn line(value: &Value, context: &str) -> String {
    let value = text(value, context);
    assert!(!value.contains('\n'), "{context}: holds an LF");
    value.to_owned()
}

enum Input {
    Plain(Vec<u8>),
    Migrated {
        version: u64,
        header: Value,
        rows: Vec<Value>,
    },
}

struct Case {
    id: String,
    input: Input,
    rows: Vec<Value>,
    entry: Map<String, Value>,
}

/// Apply a case's edits to its capture, as the TypeScript spec does.
fn build(entry: &Map<String, Value>, id: &str) -> (Vec<u8>, Vec<String>) {
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
    let mut tail = String::new();
    for edit in entry["edits"].as_array().expect("edits") {
        let edit = object(edit, id);
        match keys(edit).into_iter().collect::<Vec<_>>().as_slice() {
            ["truncate"] => {
                let count = index(&edit["truncate"], id);
                rows.truncate(count);
            }
            ["header"] => header = line(&edit["header"], id),
            ["append"] => rows.push(line(&edit["append"], id)),
            ["tail"] => tail = line(&edit["tail"], id),
            ["row", "text"] => {
                let row = index(&edit["row"], id);
                rows[row] = line(&edit["text"], id);
            }
            ["find", "replace", "row"] => {
                let row = index(&edit["row"], id);
                let find = line(&edit["find"], id);
                assert!(!find.is_empty(), "{id}: empty find");
                assert_eq!(
                    rows[row].matches(&find).count(),
                    1,
                    "{id}: find must match row {row} once"
                );
                rows[row] = rows[row].replacen(&find, &line(&edit["replace"], id), 1);
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

fn parse(text: &str, context: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{context}: {error}"))
}

fn load() -> Vec<Case> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path(
            "conformance/runtime/restored-request-derivation-cases.json",
        ))
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
            let allowed = BTreeSet::from(["id", "log", "edits", "migrated", "ts", "rust", "note"]);
            assert!(keys(&entry).is_subset(&allowed), "{id}: unknown keys");
            assert!(
                entry.get("note").is_none_or(Value::is_string),
                "{id}: invalid note"
            );
            let (input, rows) = if let Some(migrated) = entry.get("migrated") {
                assert!(
                    !entry.contains_key("log") && !entry.contains_key("edits"),
                    "{id}: a migrated case has no capture"
                );
                let migrated = object(migrated, &id);
                assert_eq!(
                    keys(migrated),
                    BTreeSet::from(["header", "rows", "version"])
                );
                let version = migrated["version"].as_u64().expect("version");
                assert!(version <= 2, "{id}: version");
                let rows: Vec<Value> = migrated["rows"]
                    .as_array()
                    .expect("rows")
                    .iter()
                    .map(|row| parse(&line(row, &id), &id))
                    .collect();
                let header = parse(&line(&migrated["header"], &id), &id);
                let input = Input::Migrated {
                    version,
                    header,
                    rows: rows.clone(),
                };
                (input, rows)
            } else {
                let (log, rows) = build(&entry, &id);
                let rows = rows.iter().map(|row| parse(row, &id)).collect();
                (Input::Plain(log), rows)
            };
            Case {
                id,
                input,
                rows,
                entry,
            }
        })
        .collect()
}

/// Resolve a JSON pointer without `~` escapes.
fn at<'a>(root: &'a [Value], pointer: &str, id: &str) -> &'a Value {
    assert!(
        pointer.starts_with('/') && !pointer.contains('~'),
        "{id}: unsupported pointer {pointer}"
    );
    let mut tokens = pointer[1..].split('/');
    let first = tokens.next().expect("token");
    let mut node = root
        .get(first.parse::<usize>().expect("row index"))
        .unwrap_or_else(|| panic!("{id}: {pointer} does not exist"));
    for token in tokens {
        node = match node {
            Value::Object(fields) => fields.get(token),
            Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        }
        .unwrap_or_else(|| panic!("{id}: {pointer} does not exist"));
    }
    node
}

/// Replace each `{ "$log": pointer }` with the value it names.
fn resolve(value: &Value, id: &str, rows: &[Value]) -> Value {
    match value {
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| resolve(item, id, rows)).collect())
        }
        Value::Object(fields) => {
            if fields.len() == 1
                && let Some(Value::String(pointer)) = fields.get("$log")
            {
                return at(rows, pointer, id).clone();
            }
            assert!(
                fields.keys().all(|key| !key.contains('$')),
                "{id}: invalid reference"
            );
            Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| (key.clone(), resolve(item, id, rows)))
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

/// `UpperCamel` as `kebab-case`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        if character.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('-');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

fn restore_limit(limit: RestoreLimit) -> String {
    format!("restore/{}", kebab(&format!("{limit:?}")))
}

/// Restore the case's input, or name the restoration limit that refused it.
fn restore(case: &Case, platform: PathPlatform) -> Result<RestoredLog, String> {
    match &case.input {
        Input::Plain(log) => {
            restore_plain_log(log, platform, SOURCE_BUDGET).map_err(|refusal| match refusal {
                RestoreRefusal::NativeSubset { limit, .. } => restore_limit(limit),
                other => panic!("{}: restoration refused: {other:?}", case.id),
            })
        }
        Input::Migrated {
            version,
            header,
            rows,
        } => {
            let migrated: MigratedV2 = if *version == 2 {
                migrate_v2_rows(header, rows, platform, SOURCE_BUDGET)
                    .unwrap_or_else(|refusal| panic!("{}: migration: {refusal:?}", case.id))
            } else {
                let codec = if *version == 0 {
                    V1CodecVersion::V0
                } else {
                    V1CodecVersion::V1
                };
                let decoded = decode_v0_v1_items(
                    header,
                    rows,
                    codec,
                    V1CodecRecovery::Strict,
                    platform,
                    SOURCE_BUDGET,
                )
                .unwrap_or_else(|refusal| panic!("{}: decode: {refusal:?}", case.id));
                migrate_released_history(&decoded)
                    .unwrap_or_else(|refusal| panic!("{}: history: {refusal:?}", case.id))
            };
            restore_migrated(&migrated, platform, SOURCE_BUDGET).map_err(|refusal| match refusal {
                MigratedRestoreRefusal::NativeSubset(MigratedRestoreLimit::Restore {
                    limit,
                    ..
                }) => restore_limit(limit),
                other => panic!("{}: migrated restoration refused: {other:?}", case.id),
            })
        }
    }
}

fn limit_name(limit: RestoredReplayLimit) -> &'static str {
    match limit {
        RestoredReplayLimit::Coordinate => "coordinate",
        RestoredReplayLimit::RepeatedCoordinate => "repeated-coordinate",
    }
}

fn cause_name(refusal: &RestoredReplayRefusal) -> &'static str {
    match refusal {
        RestoredReplayRefusal::NoLaterSettlement { .. } => "no-later-settlement",
        RestoredReplayRefusal::NoRequestHeader { .. } => "no-request-header",
        RestoredReplayRefusal::NativeSubset { .. } => "native-subset",
    }
}

fn messages_text(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .map(|request| serde_json::to_string(&request["messages"]).expect("serialize"))
        .collect()
}

/// The first mismatch of one case on one platform, if any.
fn check(case: &Case, platform: PathPlatform) -> Option<String> {
    let id = &case.id;
    let ts = object(&case.entry["ts"], id);
    let rust = case.entry.get("rust").map(|rust| object(rust, id));
    let derived = restore(case, platform).and_then(|restored| {
        let inherited = restored.stored().inherited_event_count();
        replay_restored_requests(&restored)
            .map(|requests| (inherited, requests))
            .map_err(|refusal| match refusal {
                RestoredReplayRefusal::NativeSubset { limit, .. } => limit_name(limit).to_owned(),
                refusal => format!(
                    "{}: {}",
                    cause_name(&refusal),
                    refusal.message().expect("a refusal message")
                ),
            })
    });
    match (rust.map(|rust| text(&rust["outcome"], id)), derived) {
        (Some("native-subset"), Err(limit)) if limit == text(&rust.expect("rust")["limit"], id) => {
            None
        }
        (Some("native-subset"), other) => Some(format!("{id}: expected a limit, got {other:?}")),
        (Some("rejected"), Err(refusal)) => {
            let want = format!(
                "{}: {}",
                text(&rust.expect("rust")["cause"], id),
                text(&ts["message"], id)
            );
            (refusal != want).then(|| format!("{id}: refused {refusal}, expected {want}"))
        }
        (None, Ok((inherited, requests))) => {
            let actual: Vec<Value> = requests.iter().map(|request| request.to_json()).collect();
            let want = resolve(&ts["requests"], id, &case.rows);
            let want = want.as_array().expect("requests").clone();
            if Some(inherited) != ts["inheritedEventCount"].as_u64() {
                return Some(format!("{id}: inherited count {inherited}"));
            }
            if actual != want {
                return Some(format!(
                    "{id}: requests differ\n actual: {}\n expected: {}",
                    Value::Array(actual),
                    Value::Array(want)
                ));
            }
            (messages_text(&actual) != messages_text(&want))
                .then(|| format!("{id}: message member order differs"))
        }
        (_, other) => Some(format!("{id}: unexpected outcome {other:?}")),
    }
}

#[test]
fn restored_request_derivation_cases_match_the_shared_table() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    assert_eq!(
        cases
            .iter()
            .map(|case| &case.id)
            .collect::<BTreeSet<_>>()
            .len(),
        CASE_COUNT,
        "case ids must be unique"
    );
    let overrides = |outcome: &str, key: &str| -> BTreeSet<String> {
        cases
            .iter()
            .filter_map(|case| case.entry.get("rust"))
            .filter(|rust| rust["outcome"] == outcome)
            .map(|rust| text(&rust[key], "rust").to_owned())
            .collect()
    };
    assert_eq!(
        overrides("native-subset", "limit"),
        LIMITS.iter().map(|limit| (*limit).to_owned()).collect()
    );
    assert_eq!(
        overrides("rejected", "cause"),
        CAUSES.iter().map(|cause| (*cause).to_owned()).collect()
    );
    let mut failures = Vec::new();
    for case in &cases {
        for platform in [PathPlatform::Posix, PathPlatform::Win32] {
            if let Some(failure) = check(case, platform) {
                failures.push(format!("{failure} ({platform:?})"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn replay_requests_refuses_every_seeded_case() {
    let mut seeded = 0;
    for case in load() {
        let Input::Plain(log) = &case.input else {
            continue;
        };
        let header: Value = parse(
            std::str::from_utf8(log.split(|byte| *byte == b'\n').next().expect("header"))
                .expect("utf-8"),
            &case.id,
        );
        if header["isSeeded"] == true {
            seeded += 1;
            assert_eq!(
                replay_requests(log, PathPlatform::Posix, SOURCE_BUDGET),
                Err(ReplayRefusal::Seeded),
                "{}",
                case.id
            );
        }
    }
    assert!(seeded > 0);
}
