//! Runs every shared case in `conformance/session/event-envelope-cases.json`
//! through `decode_row_envelope`, and decodes each row of the unchanged
//! request-reconstruction fixture. A case's expected outcome is its `rust`
//! override when present, otherwise the outcome of one strict released v2
//! `decodeRow` call. Agreement covers that call, not current-format row
//! admission; a case's `v3` outcome is checked only by the TypeScript spec.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    EnvelopeLimit, EnvelopeRefusal, EnvelopeRejection, NumberField, SourceEventSeqsLimit,
    UnadmittedEnvelope, decode_row_envelope,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-format-conformance/event-envelope-cases";
const ORACLE: &str = "releasedV2SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row) from packages/session/session-format-v1-to-v2/src/codec.ts, after priming rows 0 through expectedSeq - 1";
const FIXTURE: &str = "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl";
/// The TypeScript spec checks the fixture's SHA-256; this crate has no hash
/// dependency, so it pins the size and record count.
const FIXTURE_BYTES: usize = 4533;
const FIXTURE_RECORDS: usize = 17;
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 92;
/// Bounds the TypeScript oracle's priming rows.
const MAX_EXPECTED_SEQ: u64 = 16;
const LIMITS: [&str; 6] = [
    "time-float-lexeme",
    "seq-float-lexeme",
    "negative-zero-seq",
    "seq-diagnostic",
    "source-float-lexeme",
    "source-output-budget",
];
const REJECTIONS: [&str; 10] = [
    "not-object",
    "missing-field",
    "unexpected-fields",
    "type-not-string",
    "invalid-time",
    "ignorable-not-true",
    "invalid-seq",
    "source",
    "seq-gap",
    "end-seed-data-not-object",
];

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
    let text = std::fs::read_to_string(repo_path("conformance/session/event-envelope-cases.json"))
        .expect("read event-envelope-cases.json");
    serde_json::from_str(&text).expect("parse event-envelope-cases.json")
}

/// Checks a TypeScript outcome's exact keys; the spec checks its vocabulary.
fn check_outcome(outcome: &Value, context: &str) {
    let fields = object(outcome, context);
    let valid = match fields.get("outcome").and_then(Value::as_str) {
        Some("decoded") => {
            keys(fields) == BTreeSet::from(["outcome", "envelope"])
                && object(&outcome["envelope"], context).keys().all(|key| {
                    [
                        "type",
                        "seq",
                        "time",
                        "ignorable",
                        "sourceEventSeqs",
                        "surfaceOp",
                    ]
                    .contains(&key.as_str())
                })
        }
        Some("rejected") => {
            keys(fields) == BTreeSet::from(["outcome", "message"]) && outcome["message"].is_string()
        }
        Some("thrown") => {
            keys(fields) == BTreeSet::from(["outcome", "error"]) && outcome["error"] == "TypeError"
        }
        _ => false,
    };
    assert!(valid, "{context}: invalid outcome {outcome}");
}

fn rejection_kind(rejection: &EnvelopeRejection) -> &'static str {
    match rejection {
        EnvelopeRejection::NotObject => "not-object",
        EnvelopeRejection::MissingField(_) => "missing-field",
        EnvelopeRejection::UnexpectedFields { .. } => "unexpected-fields",
        EnvelopeRejection::TypeNotString => "type-not-string",
        EnvelopeRejection::InvalidTime => "invalid-time",
        EnvelopeRejection::IgnorableNotTrue => "ignorable-not-true",
        EnvelopeRejection::InvalidSeq => "invalid-seq",
        EnvelopeRejection::Source(_) => "source",
        EnvelopeRejection::SeqGap { .. } => "seq-gap",
        EnvelopeRejection::EndSeedDataNotObject => "end-seed-data-not-object",
    }
}

const fn limit_name(limit: EnvelopeLimit) -> &'static str {
    match limit {
        EnvelopeLimit::FloatLexeme(NumberField::Time) => "time-float-lexeme",
        EnvelopeLimit::FloatLexeme(NumberField::Seq) => "seq-float-lexeme",
        EnvelopeLimit::NegativeZeroSeq => "negative-zero-seq",
        EnvelopeLimit::SeqDiagnostic => "seq-diagnostic",
        EnvelopeLimit::Source(SourceEventSeqsLimit::FloatLexeme { .. }) => "source-float-lexeme",
        EnvelopeLimit::Source(SourceEventSeqsLimit::OutputBudget) => "source-output-budget",
    }
}

/// The envelope's metadata, keeping an omitted field apart from JSON `null`.
fn envelope_value(envelope: &UnadmittedEnvelope<'_>) -> Value {
    let mut value = json!({
        "type": envelope.event_type,
        "seq": envelope.seq,
        "time": envelope.time,
        "ignorable": envelope.ignorable,
    });
    if let Some(seqs) = &envelope.source_event_seqs {
        value["sourceEventSeqs"] = json!(seqs);
    }
    if let Some(operation) = envelope.surface_op {
        value["surfaceOp"] = operation.clone();
    }
    value
}

/// Asserts the envelope borrows the row's own payload values.
fn assert_borrowed(envelope: &UnadmittedEnvelope<'_>, row: &Value, context: &str) {
    assert!(
        std::ptr::eq(envelope.data, &row["data"]),
        "{context}: data is borrowed"
    );
    match (envelope.surface_op, row.get("surfaceOp")) {
        (None, None) => {}
        (Some(operation), Some(field)) => {
            assert!(
                std::ptr::eq(operation, field),
                "{context}: surfaceOp is borrowed"
            );
        }
        _ => panic!("{context}: surfaceOp presence differs from the row"),
    }
}

fn outcome_value(
    result: &Result<UnadmittedEnvelope<'_>, EnvelopeRefusal>,
    expected_seq: u64,
) -> Value {
    match result {
        Ok(envelope) => json!({"outcome": "decoded", "envelope": envelope_value(envelope)}),
        Err(EnvelopeRefusal::Rejected(rejection)) => match rejection.message(expected_seq) {
            Some(message) => json!({"outcome": "rejected", "message": message}),
            None => match rejection {
                EnvelopeRejection::UnexpectedFields { keys } => {
                    json!({"outcome": "rejected-class", "class": "unexpected-fields", "keys": keys})
                }
                other => json!({"outcome": "rejected-class", "class": rejection_kind(other)}),
            },
        },
        Err(EnvelopeRefusal::NativeSubset(limit)) => {
            json!({"outcome": "native-subset", "limit": limit_name(*limit)})
        }
        Err(EnvelopeRefusal::ExpectedSeqOutOfRange) => json!({"outcome": "caller-error"}),
    }
}

#[test]
fn shared_event_envelope_cases_match() {
    let table = table();
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from([
            "schema",
            "version",
            "oracle",
            "defaultBudget",
            "fixtureEnvelopes",
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
    let mut rejections = BTreeSet::new();
    let mut limits = BTreeSet::new();
    let mut classes = BTreeSet::new();
    for case in cases {
        let case = object(case, "case");
        let id = case
            .get("id")
            .and_then(Value::as_str)
            .expect("case id string");
        assert!(ids.insert(id), "{id}: duplicate case id");
        assert!(
            keys(case).iter().all(
                |key| ["id", "expectedSeq", "row", "budget", "ts", "rust", "v3"].contains(key)
            ),
            "{id}: unknown key"
        );
        let expected_seq = case["expectedSeq"].as_u64().expect("expectedSeq");
        assert!(
            expected_seq <= MAX_EXPECTED_SEQ,
            "{id}: expectedSeq above {MAX_EXPECTED_SEQ}"
        );
        let budget = case
            .get("budget")
            .map_or(default_budget, |budget| budget.as_u64().expect("budget"));
        let ts = case.get("ts").unwrap_or_else(|| panic!("{id}: missing ts"));
        check_outcome(ts, id);
        if let Some(v3) = case.get("v3") {
            check_outcome(v3, &format!("{id} v3"));
        }
        let row: Value = serde_json::from_str(case["row"].as_str().expect("row is JSON text"))
            .unwrap_or_else(|error| panic!("{id}: row is not one JSON value: {error}"));
        let expected = match case.get("rust") {
            Some(rust) => {
                let fields = object(rust, id);
                let valid = match rust["outcome"].as_str() {
                    Some("native-subset") => {
                        keys(fields) == BTreeSet::from(["outcome", "limit"])
                            && LIMITS.iter().any(|limit| rust["limit"] == *limit)
                    }
                    Some("rejected-class") if rust["class"] == "unexpected-fields" => {
                        keys(fields) == BTreeSet::from(["outcome", "class", "keys"])
                    }
                    Some("rejected-class") => {
                        keys(fields) == BTreeSet::from(["outcome", "class"])
                            && rust["class"] == "seq-gap"
                    }
                    _ => false,
                };
                assert!(valid, "{id}: invalid rust override {rust}");
                if rust["outcome"] == "rejected-class" {
                    classes.insert(rust["class"].as_str().expect("class"));
                }
                rust
            }
            None => {
                assert_ne!(
                    ts["outcome"], "thrown",
                    "{id}: Rust cannot claim a thrown TypeError"
                );
                ts
            }
        };
        let result = decode_row_envelope(
            &row,
            expected_seq,
            usize::try_from(budget).expect("budget fits usize"),
        );
        assert_eq!(
            &outcome_value(&result, expected_seq),
            expected,
            "{id}: outcome"
        );
        match &result {
            Ok(envelope) => assert_borrowed(envelope, &row, id),
            Err(EnvelopeRefusal::Rejected(rejection)) => {
                rejections.insert(rejection_kind(rejection));
            }
            Err(EnvelopeRefusal::NativeSubset(limit)) => {
                limits.insert(limit_name(*limit));
            }
            Err(EnvelopeRefusal::ExpectedSeqOutOfRange) => panic!("{id}: caller error"),
        }
    }
    assert_eq!(
        rejections,
        BTreeSet::from(REJECTIONS),
        "every rejection witnessed"
    );
    assert_eq!(limits, BTreeSet::from(LIMITS), "every limit witnessed");
    assert_eq!(
        classes,
        BTreeSet::from(["seq-gap", "unexpected-fields"]),
        "every class-only refusal witnessed"
    );
}

#[test]
fn fixture_envelopes_decode_like_the_released_codec() {
    let table = table();
    let fixture = object(&table["fixtureEnvelopes"], "fixtureEnvelopes");
    assert_eq!(keys(fixture), BTreeSet::from(["log", "events"]));
    assert_eq!(fixture["log"], FIXTURE);
    let expected = fixture["events"].as_array().expect("events array");

    let log = std::fs::read_to_string(repo_path(FIXTURE)).expect("read fixture log");
    assert_eq!(log.len(), FIXTURE_BYTES, "fixture size");
    let records: Vec<&str> = log
        .strip_suffix('\n')
        .expect("final LF")
        .split('\n')
        .collect();
    assert_eq!(records.len(), FIXTURE_RECORDS, "fixture records");
    let rows: Vec<Value> = records[1..]
        .iter()
        .map(|row| serde_json::from_str(row).expect("fixture row is JSON"))
        .collect();
    let mut decoded = Vec::new();
    for (seq, row) in (0_u64..).zip(&rows) {
        let envelope = decode_row_envelope(row, seq, 1024)
            .unwrap_or_else(|refusal| panic!("fixture row {seq}: {refusal:?}"));
        assert_borrowed(&envelope, row, &format!("fixture row {seq}"));
        decoded.push(envelope_value(&envelope));
    }
    assert_eq!(&decoded, expected);
    let types: BTreeSet<&str> = expected
        .iter()
        .map(|event| event["type"].as_str().expect("type"))
        .collect();
    assert_eq!(types.len(), 12);
}
