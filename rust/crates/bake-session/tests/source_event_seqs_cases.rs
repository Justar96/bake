//! Runs every shared case in `conformance/session/source-event-seqs-cases.json`
//! through `decode_source_event_seqs`, and decodes the request-reconstruction
//! fixture's physical `sourceEventSeqs` fields. A case's expected outcome is its
//! `rust` native-subset override when present, otherwise the TypeScript outcome
//! the released v2 codec produced. Agreement covers the field, not row admission.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{SourceEventSeqsLimit, SourceEventSeqsRefusal, decode_source_event_seqs};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-format-conformance/source-event-seqs-cases";
const ORACLE: &str = "releasedV2SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row) from packages/session/session-format-v1-to-v2/src/codec.ts, after rows 0 through seq - 1";
const FIXTURE: &str = "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 78;
/// Bounds the TypeScript oracle's priming rows.
const MAX_SEQ: u64 = 64;
const MESSAGES: [&str; 8] = [
    "sourceEventSeqs must be an array",
    "sourceEventSeqs member must be a non-negative safe integer",
    "sourceEventSeqs range must be a [start, end] pair",
    "sourceEventSeqs range start must be a non-negative safe integer",
    "sourceEventSeqs range end must be a non-negative safe integer",
    "sourceEventSeqs range exceeds its event seq",
    "sourceEventSeqs ranges must contain unique earlier seqs",
    "sourceEventSeqs ranges must be strictly increasing",
];
const LIMITS: [&str; 2] = ["float-lexeme", "output-budget"];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn keys(object: &Map<String, Value>) -> BTreeSet<&str> {
    object.keys().map(String::as_str).collect()
}

fn object<'a>(value: &'a Value, context: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected an object, got {value}"))
}

fn table() -> Value {
    let text = std::fs::read_to_string(repo_path(
        "conformance/session/source-event-seqs-cases.json",
    ))
    .expect("read source-event-seqs-cases.json");
    serde_json::from_str(&text).expect("parse source-event-seqs-cases.json")
}

/// Checks one TypeScript outcome's exact keys and vocabulary.
fn check_ts(ts: &Value, id: &str) {
    let ts_object = object(ts, id);
    let valid = match ts_object.get("outcome").and_then(Value::as_str) {
        Some("absent") => keys(ts_object) == BTreeSet::from(["outcome"]),
        Some("decoded") => {
            keys(ts_object) == BTreeSet::from(["outcome", "seqs"])
                && ts["seqs"]
                    .as_array()
                    .is_some_and(|seqs| seqs.iter().all(Value::is_u64))
        }
        Some("rejected") => {
            keys(ts_object) == BTreeSet::from(["outcome", "message"])
                && MESSAGES.iter().any(|message| ts["message"] == *message)
        }
        _ => false,
    };
    assert!(valid, "{id}: invalid ts outcome {ts}");
}

fn outcome_value(result: &Result<Option<Vec<u64>>, SourceEventSeqsRefusal>) -> Value {
    match result {
        Ok(None) => json!({"outcome": "absent"}),
        Ok(Some(seqs)) => json!({"outcome": "decoded", "seqs": seqs}),
        Err(SourceEventSeqsRefusal::Rejected(rejection)) => {
            json!({"outcome": "rejected", "message": rejection.message()})
        }
        Err(SourceEventSeqsRefusal::NativeSubset(limit)) => {
            let limit = match limit {
                SourceEventSeqsLimit::FloatLexeme { .. } => "float-lexeme",
                SourceEventSeqsLimit::OutputBudget => "output-budget",
            };
            json!({"outcome": "native-subset", "limit": limit})
        }
        Err(SourceEventSeqsRefusal::EventSeqOutOfRange) => json!({"outcome": "caller-error"}),
    }
}

#[test]
fn shared_source_event_seqs_cases_match() {
    let table = table();
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from([
            "schema",
            "version",
            "oracle",
            "defaultBudget",
            "fixtureReferences",
            "cases"
        ])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    let default_budget = table["defaultBudget"].as_u64().expect("defaultBudget");

    let cases = table["cases"].as_array().expect("cases array");
    assert_eq!(cases.len(), CASE_COUNT, "case count");
    let mut ids = BTreeSet::new();
    let mut messages = BTreeSet::new();
    let mut limits = BTreeSet::new();
    for case in cases {
        let case = object(case, "case");
        let id = case
            .get("id")
            .and_then(Value::as_str)
            .expect("case id string");
        assert!(ids.insert(id), "{id}: duplicate case id");
        assert!(
            keys(case)
                .iter()
                .all(|key| ["id", "seq", "field", "budget", "ts", "rust"].contains(key)),
            "{id}: unknown key"
        );
        let seq = case["seq"].as_u64().expect("seq");
        assert!(seq <= MAX_SEQ, "{id}: seq above {MAX_SEQ}");
        let budget = case
            .get("budget")
            .map_or(default_budget, |budget| budget.as_u64().expect("budget"));
        let ts = case.get("ts").unwrap_or_else(|| panic!("{id}: missing ts"));
        check_ts(ts, id);
        if let Some(message) = ts.get("message") {
            messages.insert(message.as_str().expect("message"));
        }
        let field: Option<Value> = case.get("field").map(|field| {
            serde_json::from_str(field.as_str().expect("field is JSON text"))
                .unwrap_or_else(|error| panic!("{id}: field is not one JSON value: {error}"))
        });
        assert_eq!(
            field.is_none(),
            ts["outcome"] == "absent",
            "{id}: field is omitted exactly when the outcome is absent"
        );
        if let Some(rust) = case.get("rust") {
            let rust_object = object(rust, id);
            assert!(
                keys(rust_object) == BTreeSet::from(["outcome", "limit"])
                    && rust["outcome"] == "native-subset"
                    && LIMITS.iter().any(|limit| rust["limit"] == *limit),
                "{id}: rust may only name a native-subset limit"
            );
            limits.insert(rust["limit"].as_str().expect("limit"));
        }
        let expected = case.get("rust").unwrap_or(ts);
        let result = decode_source_event_seqs(
            field.as_ref(),
            seq,
            usize::try_from(budget).expect("budget fits usize"),
        );
        assert_eq!(&outcome_value(&result), expected, "{id}: outcome");
    }
    assert_eq!(
        messages,
        BTreeSet::from(MESSAGES),
        "every message witnessed"
    );
    assert_eq!(limits, BTreeSet::from(LIMITS), "every limit witnessed");
}

#[test]
fn fixture_references_decode_like_the_released_codec() {
    let table = table();
    let references = object(&table["fixtureReferences"], "fixtureReferences");
    assert_eq!(keys(references), BTreeSet::from(["log", "events"]));
    assert_eq!(references["log"], FIXTURE);
    let expected = references["events"].as_array().expect("events array");
    assert!(!expected.is_empty());

    let log = std::fs::read_to_string(repo_path(FIXTURE)).expect("read fixture log");
    let mut rows = log.lines();
    rows.next().expect("header record");
    let mut decoded = Vec::new();
    for row in rows {
        let row: Value = serde_json::from_str(row).expect("fixture row is JSON");
        let seq = row["seq"].as_u64().expect("row seq");
        let field = row.get("sourceEventSeqs");
        if let Some(seqs) =
            decode_source_event_seqs(field, seq, 1024).expect("fixture field decodes")
        {
            decoded.push(json!({"seq": seq, "type": row["type"], "sourceEventSeqs": seqs}));
        }
    }
    assert_eq!(&decoded, expected);
}
