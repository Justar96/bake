//! Runs every shared case in `conformance/session/log-scan-cases.json`
//! through `scan_log`. A case's expected outcome is its `rust` override when
//! present, otherwise what the TypeScript `scanLog` does with the same bytes.
//! Numbers compare by `Number::as_f64`, the JavaScript value of the lexeme.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bake_session::{
    HeaderRefusal, PathPlatform, Rejection, ScanLimit, ScanRefusal, ScannedLog, scan_log,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/log-scan-cases";
const ORACLE: &str = "scanLog(log) from packages/session/session-persistence-jsonl/src/format.ts";
const FIXTURE: &str = "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl";
/// The TypeScript spec checks the fixture's SHA-256; this crate pins its size.
const FIXTURE_BYTES: usize = 4533;
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 76;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 4] = ["invalid-utf8", "json-parser", "number-lexeme", "codec"];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn keys(object: &Map<String, Value>) -> BTreeSet<&str> {
    object.keys().map(String::as_str).collect()
}

fn table() -> Vec<Value> {
    let text = std::fs::read_to_string(repo_path("conformance/session/log-scan-cases.json"))
        .expect("read log-scan-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse log-scan-cases.json");
    let fields = table.as_object().expect("table object");
    assert_eq!(
        keys(fields),
        BTreeSet::from(["schema", "version", "oracle", "cases"])
    );
    assert_eq!(fields["schema"], SCHEMA);
    assert_eq!(fields["version"], 1);
    assert_eq!(fields["oracle"], ORACLE);
    fields["cases"].as_array().expect("cases").clone()
}

/// The case's bytes: `lines` each followed by LF, then `tail`; or hex; or the fixture.
fn case_log(case: &Map<String, Value>, id: &str) -> Vec<u8> {
    let sources: Vec<_> = ["lines", "bytesHex", "fixture"]
        .into_iter()
        .filter(|key| case.contains_key(*key))
        .collect();
    assert!(
        sources.len() == 1 && (!case.contains_key("tail") || sources[0] == "lines"),
        "{id}: exactly one log source; tail requires lines"
    );
    if let Some(fixture) = case.get("fixture") {
        assert_eq!(fixture, FIXTURE, "{id}: fixture");
        let log = std::fs::read(repo_path(FIXTURE)).expect("read fixture");
        assert_eq!(log.len(), FIXTURE_BYTES, "fixture size");
        return log;
    }
    if let Some(hex) = case.get("bytesHex") {
        let hex = hex.as_str().expect("hex text");
        assert!(
            hex.len() % 2 == 0 && !case.contains_key("lines"),
            "{id}: bytesHex"
        );
        return (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex byte"))
            .collect();
    }
    let mut log = Vec::new();
    for line in case["lines"].as_array().expect("lines") {
        let line = line.as_str().expect("line text");
        assert!(!line.contains('\n'), "{id}: a line holds LF");
        log.extend_from_slice(line.as_bytes());
        log.push(b'\n');
    }
    if let Some(tail) = case.get("tail") {
        let tail = tail.as_str().expect("tail text");
        assert!(!tail.contains('\n'), "{id}: the tail holds LF");
        log.extend_from_slice(tail.as_bytes());
    }
    log
}

const fn limit_name(limit: ScanLimit) -> &'static str {
    match limit {
        ScanLimit::InvalidUtf8 => "invalid-utf8",
        ScanLimit::JsonParser => "json-parser",
        ScanLimit::NumberLexeme => "number-lexeme",
        ScanLimit::Codec(_) => "codec",
        ScanLimit::EventCount => "event-count",
    }
}

/// The TypeScript error class a refusal claims.
fn class(refusal: &ScanRefusal) -> &'static str {
    match refusal {
        ScanRefusal::Header(HeaderRefusal::Rejected(_)) | ScanRefusal::Corrupt { .. } => "Error",
        ScanRefusal::Header(HeaderRefusal::UnsupportedVersion { .. })
        | ScanRefusal::Unsupported { .. } => "SessionFormatUnsupportedError",
        ScanRefusal::Structural { .. } | ScanRefusal::Finish(_) => "SessionFormatError",
        ScanRefusal::Header(HeaderRefusal::NativeSubset(_)) | ScanRefusal::NativeSubset { .. } => {
            panic!("a limit claims no class: {refusal:?}")
        }
    }
}

/// TypeScript's message: the header reasons are fixed strings.
fn message(refusal: &ScanRefusal) -> Option<String> {
    let header = match refusal {
        ScanRefusal::Header(HeaderRefusal::Rejected(rejection)) => rejection,
        _ => return refusal.message(),
    };
    Some(
        match header {
            Rejection::Framing => "empty or header-less session log",
            Rejection::Json => "corrupt session log: header line is not valid JSON",
            Rejection::NotObject => "corrupt session log: first line is not a JSON object",
            Rejection::RetiredPolicyFields => "session header uses retired policy baseline fields",
            Rejection::NotSessionHeader => {
                "corrupt session log: first line is not a session header"
            }
        }
        .to_owned(),
    )
}

fn hex_bits(value: &Value) -> String {
    format!("{:016x}", value.as_f64().expect("a number").to_bits())
}

/// The scan's header, cut, offset, rows, decoded events, and numbers.
fn assert_scanned(scan: &ScannedLog, log: &[u8], ts: &Value, id: &str) {
    let header = scan.header();
    let mut meta = Map::new();
    meta.insert("version".into(), 3.into());
    meta.insert("id".into(), header.id.clone().into());
    meta.insert("createdAt".into(), header.created_at.into());
    meta.insert("isSeeded".into(), header.is_seeded.into());
    meta.insert("delegationDepth".into(), header.delegation_depth.into());
    for (key, value) in [
        ("cwd", &header.cwd),
        ("parentSession", &header.parent_session),
        ("agentPreset", &header.agent_preset),
    ] {
        if let Some(value) = value {
            meta.insert(key.into(), value.clone().into());
        }
    }
    assert!(header.origin.is_none(), "{id}: no case has an origin");
    assert_eq!(Value::Object(meta), ts["header"], "{id}: header");
    assert_eq!(
        scan.inherited_event_count(),
        ts["inheritedEventCount"],
        "{id}: cut"
    );
    assert_eq!(
        scan.committed_bytes(),
        ts["committedBytes"].as_u64().expect("offset") as usize,
        "{id}: committed bytes"
    );
    let count = ts["events"].as_u64().expect("event count") as usize;
    let records: Vec<Value> = log
        .split(|byte| *byte == b'\n')
        .skip(1)
        .take(count)
        .map(|record| serde_json::from_slice(record).expect("decoded record"))
        .collect();
    assert_eq!(scan.rows(), records.as_slice(), "{id}: rows");
    let sources = ts
        .get("sources")
        .map(|value| value.as_object().expect("source expectations"));
    let mut events = 0;
    for ((event, row), seq) in scan.events().zip(&records).zip(0u64..) {
        let envelope = event.envelope();
        assert_eq!(envelope.event_type, row["type"], "{id}: type");
        assert_eq!(envelope.seq, seq);
        assert_eq!(
            Some(envelope.time as f64),
            row["time"].as_f64(),
            "{id}: time"
        );
        assert_eq!(
            envelope.ignorable,
            row.get("ignorable") == Some(&Value::Bool(true))
        );
        assert!(std::ptr::eq(envelope.data, &scan.rows()[events]["data"]));
        assert_eq!(envelope.surface_op, row.get("surfaceOp"), "{id}: surfaceOp");
        let expected = sources
            .and_then(|sources| sources.get(&events.to_string()))
            .or_else(|| row.get("sourceEventSeqs"))
            .map(|list| {
                list.as_array()
                    .expect("source list")
                    .iter()
                    .map(|seq| seq.as_u64().expect("expanded seq"))
                    .collect::<Vec<_>>()
            });
        assert_eq!(envelope.source_event_seqs, expected, "{id}: sources");
        events += 1;
    }
    assert_eq!(events, count, "{id}: events");
    for number in ts
        .get("numbers")
        .map(|value| value.as_array().expect("number expectations"))
        .into_iter()
        .flatten()
    {
        let event = number["event"].as_u64().expect("event index") as usize;
        let pointer = number["pointer"].as_str().expect("pointer");
        let value = scan.rows()[event].pointer(pointer).expect("number present");
        assert_eq!(hex_bits(value), number["bits"], "{id}: {pointer}");
    }
}

#[test]
fn shared_cases_scan_like_scan_log() {
    let cases = table();
    assert_eq!(cases.len(), CASE_COUNT, "case count");
    let mut ids = BTreeSet::new();
    let mut limit_outcomes: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for case in &cases {
        let fields = case.as_object().expect("case object");
        let id = case["id"].as_str().expect("case id");
        assert!(ids.insert(id), "{id}: duplicate id");
        let allowed = BTreeSet::from(["id", "lines", "tail", "bytesHex", "fixture", "ts", "rust"]);
        assert!(keys(fields).is_subset(&allowed), "{id}: unknown keys");
        let log = case_log(fields, id);
        let actual = scan_log(&log, PathPlatform::host(), SOURCE_BUDGET);
        let ts = &case["ts"];
        let ts_outcome = ts["outcome"].as_str().expect("ts outcome");
        match case.get("rust") {
            Some(rust) if rust["outcome"] == "native-subset" => {
                let name = rust["limit"].as_str().expect("limit");
                assert!(LIMITS.contains(&name), "{id}: unknown limit {name}");
                match actual {
                    Err(ScanRefusal::NativeSubset { limit, .. }) => {
                        assert_eq!(limit_name(limit), name, "{id}");
                    }
                    other => panic!("{id}: expected limit {name}, got {other:?}"),
                }
                limit_outcomes
                    .entry(name)
                    .or_default()
                    .insert(ts_outcome.to_owned());
            }
            Some(rust) => {
                assert_eq!(
                    rust,
                    &serde_json::json!({"outcome": "thrown-class"}),
                    "{id}"
                );
                let refusal = actual.expect_err(id);
                assert_eq!(class(&refusal), ts["class"], "{id}: class");
                assert_eq!(message(&refusal), None, "{id}: class only");
            }
            None if ts_outcome == "thrown" => {
                let refusal = actual.expect_err(id);
                assert_eq!(class(&refusal), ts["class"], "{id}: class");
                assert_eq!(message(&refusal).as_deref(), ts["message"].as_str(), "{id}");
            }
            None => {
                assert_eq!(ts_outcome, "scanned", "{id}: expected scanned outcome");
                let scan = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                assert_scanned(&scan, &log, ts, id);
            }
        }
    }
    for limit in LIMITS {
        assert_eq!(
            limit_outcomes.get(limit).cloned().unwrap_or_default(),
            BTreeSet::from(["scanned".to_owned(), "thrown".to_owned()]),
            "{limit} covers accepted and rejected input"
        );
    }
}
