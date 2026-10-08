//! Runs every shared case in `conformance/session/plain-append-cases.json`
//! through `PlainAppendLog`, over the same parsed values the TypeScript spec
//! passes to the JSONL backend. A case creates a log from a header, or opens
//! one from bytes, then applies its operations in order. After each
//! operation the log's bytes must equal the hand-written `log`, or be absent
//! where it is `null`. A thrown outcome maps to the refusal Rust claims: the
//! exact contiguity or lossless-snapshot message, or `Unadmitted` for any
//! other throw. A `rust` override names a native limit, ends the case, and
//! skips that operation's bytes; on a create it skips every operation. A `scan` reads the final bytes back with
//! `scan_log`, and the reopened log's cursor must equal the model's. Nothing
//! here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    AppendLimit, AppendRefusal, CreateLimit, CreateRefusal, PathPlatform, PlainAppendLog, scan_log,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/plain-append-cases";
const ORACLE: &str = "create or write-open a handle of the JSONL backend with compression none in an owned temporary root, apply append and flush in order, and read the log file's bytes after each operation; scanLog reads the final bytes back";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 45;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 3] = ["seq-value", "encode", "empty-id"];
const CLASSES: [&str; 3] = ["Error", "TypeError", "SessionFormatError"];
const SEQ_MISMATCH: &str = "append seq mismatch for ";
const NOT_LOSSLESS: &str = "session event batch is not losslessly JSON-serializable because it contains non-JSON-serializable data";

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

fn load() -> Vec<Map<String, Value>> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/plain-append-cases.json"))
            .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "oracle", "schema", "version"])
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
    table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|entry| object(entry, "case").clone())
        .collect()
}

/// A thrown `ts` outcome's class and optional message, checked against the schema.
fn thrown<'a>(ts: &'a Map<String, Value>, id: &str) -> (&'a str, Option<&'a str>) {
    assert_eq!(ts["outcome"], "thrown", "{id}");
    let class = text(&ts["class"], id);
    assert!(CLASSES.contains(&class), "{id}: class {class}");
    let message = ts.get("message").map(|message| text(message, id));
    let expected_keys = if message.is_some() { 3 } else { 2 };
    assert_eq!(keys(ts).len(), expected_keys, "{id}: thrown keys");
    (class, message)
}

/// The refusal Rust claims for a TypeScript throw.
fn expected_refusal(class: &str, message: Option<&str>, id: &str) -> AppendRefusal {
    let message =
        message.unwrap_or_else(|| panic!("{id}: a throw without a message needs a limit"));
    if class == "Error" && message.starts_with(SEQ_MISMATCH) {
        AppendRefusal::SeqMismatch {
            message: message.to_owned(),
        }
    } else if class == "TypeError" && message == NOT_LOSSLESS {
        AppendRefusal::NotLossless
    } else {
        AppendRefusal::Unadmitted
    }
}

fn limit_matches(name: &str, actual: &Result<(), AppendRefusal>) -> bool {
    match (name, actual) {
        ("seq-value", Err(AppendRefusal::NativeSubset(AppendLimit::SeqValue))) => true,
        ("encode", Err(AppendRefusal::NativeSubset(AppendLimit::Encode(_)))) => true,
        (other, _) => {
            assert!(LIMITS.contains(&other), "unknown limit {other}");
            false
        }
    }
}

fn create_limit_matches(name: &str, actual: &Result<PlainAppendLog, CreateRefusal>) -> bool {
    match (name, actual) {
        ("empty-id", Err(CreateRefusal::NativeSubset(CreateLimit::EmptyId))) => true,
        ("encode", Err(CreateRefusal::NativeSubset(CreateLimit::Encode(_)))) => true,
        (other, _) => {
            assert!(LIMITS.contains(&other), "unknown limit {other}");
            false
        }
    }
}

/// A `rust` override's limit name, checked against the schema.
fn override_limit<'a>(rust: &'a Value, context: &str) -> &'a str {
    let rust = object(rust, context);
    assert_eq!(
        keys(rust),
        BTreeSet::from(["limit", "outcome"]),
        "{context}"
    );
    assert_eq!(rust["outcome"], "native-subset", "{context}");
    text(&rust["limit"], context)
}

/// The operation's expected log text, or `None` where no file exists.
fn expected_log<'a>(op: &'a Map<String, Value>, id: &str) -> Option<&'a str> {
    match &op["log"] {
        Value::Null => None,
        log => Some(text(log, id)),
    }
}

/// Create or open the case's log; `None` when the create is refused or
/// limited as expected, recording a limit in `limits`.
fn start(
    entry: &Map<String, Value>,
    id: &str,
    limits: &mut BTreeSet<String>,
) -> Option<PlainAppendLog> {
    if let Some(create) = entry.get("create") {
        assert!(!entry.contains_key("open"), "{id}: create or open");
        let create = object(create, id);
        assert!(
            keys(create).is_subset(&BTreeSet::from([
                "header",
                "inheritedEventCount",
                "rust",
                "ts"
            ])),
            "{id}: create keys"
        );
        let count = create
            .get("inheritedEventCount")
            .map(|count| count.as_u64().unwrap_or_else(|| panic!("{id}: count")));
        let actual = PlainAppendLog::create(&create["header"], count);
        if let Some(rust) = create.get("rust") {
            assert!(
                !create.contains_key("ts"),
                "{id}: a limited create succeeds"
            );
            let name = override_limit(rust, id);
            assert!(create_limit_matches(name, &actual), "{id}: {actual:?}");
            assert!(
                !entry.contains_key("scan"),
                "{id}: a limited case scans nothing"
            );
            limits.insert(name.to_owned());
            return None;
        }
        return match create.get("ts") {
            None => Some(actual.unwrap_or_else(|refusal| panic!("{id}: create {refusal:?}"))),
            Some(ts) => {
                thrown(object(ts, id), id);
                assert_eq!(actual, Err(CreateRefusal::Unadmitted), "{id}");
                assert!(entry["ops"].as_array().expect("ops").is_empty(), "{id}");
                None
            }
        };
    }
    let log = text(&entry["open"], id).as_bytes();
    Some(
        PlainAppendLog::open(log, PathPlatform::host(), SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: open {refusal:?}")),
    )
}

fn check_scan(entry: &Map<String, Value>, log: &PlainAppendLog, id: &str) {
    let Some(scan) = entry.get("scan") else {
        return;
    };
    let scan = object(scan, id);
    assert_eq!(
        keys(scan),
        BTreeSet::from(["committedBytes", "events", "inheritedEventCount"]),
        "{id}"
    );
    let bytes = log
        .bytes()
        .unwrap_or_else(|| panic!("{id}: scan needs a file"));
    let scanned = scan_log(bytes, PathPlatform::host(), SOURCE_BUDGET)
        .unwrap_or_else(|refusal| panic!("{id}: scan {refusal:?}"));
    assert_eq!(
        Some(scanned.rows().len() as u64),
        scan["events"].as_u64(),
        "{id}"
    );
    assert_eq!(
        Some(scanned.committed_bytes() as u64),
        scan["committedBytes"].as_u64(),
        "{id}"
    );
    assert_eq!(
        Some(scanned.inherited_event_count()),
        scan["inheritedEventCount"].as_u64(),
        "{id}"
    );
    let reopened = PlainAppendLog::open(bytes, PathPlatform::host(), SOURCE_BUDGET)
        .unwrap_or_else(|refusal| panic!("{id}: reopen {refusal:?}"));
    assert_eq!(reopened.cursor(), log.cursor(), "{id}: cursor round trip");
    assert_eq!(reopened.id(), log.id(), "{id}");
    assert_eq!(
        reopened.inherited_event_count(),
        log.inherited_event_count(),
        "{id}"
    );
}

#[test]
fn shared_cases_write_like_the_typescript_backend() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases
        .iter()
        .map(|entry| text(&entry["id"], "case id"))
        .collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut limits = BTreeSet::new();
    let mut classes = BTreeSet::new();
    let mut refusals = BTreeSet::new();
    for entry in &cases {
        let id = text(&entry["id"], "case id");
        assert!(
            keys(entry).is_subset(&BTreeSet::from([
                "id", "create", "open", "ops", "scan", "note"
            ])),
            "{id}: unknown keys"
        );
        assert!(
            entry.get("note").is_none_or(Value::is_string),
            "{id}: invalid note"
        );
        if let Some(Value::Object(create)) = entry.get("create")
            && let Some(ts) = create.get("ts")
        {
            classes.insert(thrown(object(ts, id), id).0.to_owned());
        }
        let Some(mut log) = start(entry, id, &mut limits) else {
            continue;
        };
        let ops = entry["ops"].as_array().expect("ops");
        let mut limited = false;
        for (index, op) in ops.iter().enumerate() {
            let context = format!("{id} op {index}");
            assert!(!limited, "{context}: a limit ends the case");
            let op = object(op, &context);
            let ts = object(&op["ts"], &context);
            let actual = match text(&op["op"], &context) {
                "flush" => {
                    assert_eq!(keys(op), BTreeSet::from(["log", "op", "ts"]), "{context}");
                    assert_eq!(keys(ts), BTreeSet::from(["outcome"]), "{context}");
                    assert_eq!(ts["outcome"], "flushed", "{context}");
                    log.flush();
                    Ok(())
                }
                "append" => {
                    assert!(
                        keys(op).is_subset(&BTreeSet::from(["events", "log", "op", "rust", "ts"])),
                        "{context}"
                    );
                    log.append(op["events"].as_array().expect("events"))
                }
                other => panic!("{context}: unknown op {other}"),
            };
            if ts["outcome"] == "thrown" {
                classes.insert(thrown(ts, &context).0.to_owned());
            } else if ts["outcome"] != "flushed" {
                assert_eq!(keys(ts), BTreeSet::from(["outcome"]), "{context}");
                assert_eq!(ts["outcome"], "appended", "{context}");
            }
            if let Some(rust) = op.get("rust") {
                let name = override_limit(rust, &context);
                assert!(limit_matches(name, &actual), "{context}: {actual:?}");
                limits.insert(name.to_owned());
                limited = true;
                continue;
            }
            match ts["outcome"].as_str() {
                Some("appended" | "flushed") => assert_eq!(actual, Ok(()), "{context}"),
                _ => {
                    let (class, message) = thrown(ts, &context);
                    let expected = expected_refusal(class, message, &context);
                    let kind = match &expected {
                        AppendRefusal::NotLossless => "not-lossless",
                        AppendRefusal::SeqMismatch { .. } => "seq-mismatch",
                        _ => "unadmitted",
                    };
                    refusals.insert(kind);
                    if kind != "unadmitted" {
                        assert_eq!(expected.message(), message, "{context}");
                    }
                    assert_eq!(actual, Err(expected), "{context}");
                }
            }
            let written = log.bytes().map(String::from_utf8_lossy);
            assert_eq!(written.as_deref(), expected_log(op, &context), "{context}");
        }
        if limited {
            assert!(
                !entry.contains_key("scan"),
                "{id}: a limited case scans nothing"
            );
        } else {
            check_scan(entry, &log, id);
        }
    }
    assert_eq!(
        limits,
        LIMITS.iter().map(|name| (*name).to_owned()).collect(),
        "every limit is witnessed"
    );
    assert_eq!(
        classes,
        CLASSES.iter().map(|name| (*name).to_owned()).collect(),
        "every class is witnessed"
    );
    assert_eq!(
        refusals,
        BTreeSet::from(["not-lossless", "seq-mismatch", "unadmitted"]),
        "every refusal is witnessed"
    );
}
