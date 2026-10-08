//! Runs every shared case in `conformance/session/row-encode-cases.json`
//! through `encode_header_line` or `encode_event_line`, over the same parsed
//! values the TypeScript spec encodes. A case's expected outcome is its
//! `rust` override when present, otherwise the hand-written `ts` line, or
//! `Unadmitted` where TypeScript throws. Every line Rust writes must read
//! back: a header through `read_header_record`, an event through
//! `decode_v3_row`, and a log case's joined lines through `scan_log`.
//! Nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    EncodeLimit, EncodeRefusal, PathPlatform, decode_v3_row, encode_event_line, encode_header_line,
    read_header_record, scan_log,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/row-encode-cases";
const ORACLE: &str = "JSON.stringify(toHeaderLine(header, inheritedEventCount)) and eventLine(event) from packages/session/session-persistence-jsonl/src/format.ts; log cases join the lines and scanLog reads them back";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 95;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 5] = [
    "float-number",
    "type-error",
    "source-coercion",
    "unreadable-row",
    "codec",
];
const CLASSES: [&str; 4] = [
    "Error",
    "TypeError",
    "SessionFormatError",
    "SessionFormatUnsupportedMigrationError",
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

fn load() -> Vec<Map<String, Value>> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/row-encode-cases.json")).expect("read table"),
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

/// The refusal a `rust` override names.
fn limit_matches(name: &str, actual: &Result<String, EncodeRefusal>) -> bool {
    let Err(EncodeRefusal::NativeSubset(limit)) = actual else {
        return false;
    };
    match name {
        "float-number" => *limit == EncodeLimit::FloatNumber,
        "type-error" => *limit == EncodeLimit::TypeError,
        "source-coercion" => *limit == EncodeLimit::SourceCoercion,
        "unreadable-row" => *limit == EncodeLimit::UnreadableRow,
        "codec" => matches!(limit, EncodeLimit::Codec(_)),
        other => panic!("unknown limit {other}"),
    }
}

/// The inherited count a case passes, absent when the member is.
fn inherited(entry: &Map<String, Value>, id: &str) -> Option<u64> {
    entry
        .get("inheritedEventCount")
        .map(|count| count.as_u64().unwrap_or_else(|| panic!("{id}: count")))
}

/// A written line must read back through the strict decoders.
fn reads_back(kind: &str, input: &Value, line: &str, id: &str) {
    assert!(!line.contains('\n'), "{id}: a line holds no LF");
    match kind {
        "header" => {
            read_header_record(format!("{line}\n").as_bytes(), PathPlatform::host())
                .unwrap_or_else(|refusal| panic!("{id}: header reads back, got {refusal:?}"));
        }
        _ => {
            let row: Value = serde_json::from_str(line).expect("written line parses");
            let seq = input["seq"]
                .as_u64()
                .expect("written event has a count seq");
            let decoded = decode_v3_row(&row, seq, SOURCE_BUDGET)
                .unwrap_or_else(|refusal| panic!("{id}: row reads back, got {refusal:?}"));
            let sources = input
                .get("sourceEventSeqs")
                .map(|sources| serde_json::from_value::<Vec<u64>>(sources.clone()).expect("seqs"));
            assert_eq!(decoded.envelope().source_event_seqs, sources, "{id}");
        }
    }
}

fn check_log(entry: &Map<String, Value>, id: &str) {
    let ts = object(&entry["ts"], id);
    assert_eq!(
        keys(ts),
        BTreeSet::from(["inheritedEventCount", "log", "outcome"]),
        "{id}"
    );
    assert_eq!(ts["outcome"], "scanned", "{id}");
    let mut log = encode_header_line(&entry["header"], inherited(entry, id))
        .unwrap_or_else(|refusal| panic!("{id}: header encodes, got {refusal:?}"));
    log.push('\n');
    let events = entry["events"].as_array().expect("events");
    for event in events {
        let line = encode_event_line(event)
            .unwrap_or_else(|refusal| panic!("{id}: event encodes, got {refusal:?}"));
        log.push_str(&line);
        log.push('\n');
    }
    assert_eq!(log, text(&ts["log"], id), "{id}");
    let scanned = scan_log(log.as_bytes(), PathPlatform::host(), SOURCE_BUDGET)
        .unwrap_or_else(|refusal| panic!("{id}: log scans, got {refusal:?}"));
    assert_eq!(scanned.committed_bytes(), log.len(), "{id}");
    assert_eq!(
        Some(scanned.inherited_event_count()),
        ts["inheritedEventCount"].as_u64(),
        "{id}"
    );
    assert_eq!(
        scanned.header().id,
        text(&entry["header"]["id"], id),
        "{id}"
    );
    assert_eq!(scanned.rows().len(), events.len(), "{id}");
    for ((row, decoded), event) in scanned.rows().iter().zip(scanned.events()).zip(events) {
        let mut logical = object(row, id).clone();
        if let Some(sources) = event.get("sourceEventSeqs") {
            let expanded = decoded
                .envelope()
                .source_event_seqs
                .clone()
                .expect("decoded sources");
            assert_eq!(Value::from(expanded), *sources, "{id}");
            logical.insert("sourceEventSeqs".to_owned(), sources.clone());
        }
        assert_eq!(Value::Object(logical), *event, "{id}");
    }
}

#[test]
fn shared_cases_encode_like_the_typescript_writer() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases
        .iter()
        .map(|entry| text(&entry["id"], "case id"))
        .collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut limits = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for entry in &cases {
        let id = text(&entry["id"], "case id");
        let kind = text(&entry["kind"], id);
        kinds.insert(kind.to_owned());
        let allowed = match kind {
            "header" => vec![
                "id",
                "kind",
                "header",
                "inheritedEventCount",
                "ts",
                "rust",
                "note",
            ],
            "event" => vec!["id", "kind", "event", "ts", "rust", "note"],
            "log" => vec![
                "id",
                "kind",
                "header",
                "inheritedEventCount",
                "events",
                "ts",
                "note",
            ],
            other => panic!("{id}: unknown kind {other}"),
        };
        assert!(
            keys(entry).is_subset(&allowed.into_iter().collect()),
            "{id}: unknown keys"
        );
        assert!(
            entry.get("note").is_none_or(Value::is_string),
            "{id}: invalid note"
        );
        if kind == "log" {
            check_log(entry, id);
            continue;
        }
        let input = &entry[kind];
        let actual = if kind == "header" {
            encode_header_line(input, inherited(entry, id))
        } else {
            encode_event_line(input)
        };
        let ts = object(&entry["ts"], id);
        match text(&ts["outcome"], id) {
            "encoded" => assert_eq!(keys(ts), BTreeSet::from(["line", "outcome"]), "{id}"),
            "thrown" => {
                assert!(CLASSES.contains(&text(&ts["class"], id)), "{id}: class");
                let has_message = ts.contains_key("message");
                assert_eq!(
                    keys(ts).len(),
                    if has_message { 3 } else { 2 },
                    "{id}: thrown keys"
                );
                // Only an engine TypeError goes without its message, and Rust
                // does not decide one.
                assert!(
                    has_message || entry.contains_key("rust"),
                    "{id}: a TypeError without a message needs a limit"
                );
            }
            other => panic!("{id}: unknown outcome {other}"),
        }
        match entry.get("rust") {
            Some(rust) => {
                let rust = object(rust, id);
                assert_eq!(keys(rust), BTreeSet::from(["limit", "outcome"]), "{id}");
                assert_eq!(rust["outcome"], "native-subset", "{id}");
                let name = text(&rust["limit"], id);
                assert!(LIMITS.contains(&name), "{id}: unknown limit {name}");
                assert!(limit_matches(name, &actual), "{id}: {actual:?}");
                limits.insert(name);
            }
            None if ts["outcome"] == "encoded" => {
                let line = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                assert_eq!(line, text(&ts["line"], id), "{id}");
                reads_back(kind, input, &line, id);
            }
            None => assert_eq!(actual, Err(EncodeRefusal::Unadmitted), "{id}"),
        }
    }
    assert_eq!(limits, BTreeSet::from(LIMITS), "every limit is witnessed");
    assert_eq!(
        kinds,
        BTreeSet::from(["event".to_owned(), "header".to_owned(), "log".to_owned()])
    );
}
