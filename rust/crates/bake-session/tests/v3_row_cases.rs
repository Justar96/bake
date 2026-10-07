//! Runs every shared case in `conformance/session/v3-row-cases.json` through
//! `decode_v3_row`, classifies every listed vocabulary name by behavior, and
//! decodes each row of the unchanged request-reconstruction fixture and its
//! mutants. A case's expected outcome is its `rust` override when present,
//! otherwise the outcome of one strict V3 codec `decodeRow` call.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    Coordinate, Endpoint, EnvelopeLimit, EventRejection, SourceEventSeqsLimit, StructuralRejection,
    UnadmittedEnvelope, V3CodecEvent, V3Limit, V3NumberField, V3Rejection, V3RowRefusal,
    V3Unsupported, decode_row_envelope, decode_v3_row,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-format-conformance/v3-row-cases";
const ORACLE: &str = "releasedV3SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row) from packages/session/session-format-v2-to-v3/src/codec.ts, after priming rows 0 through expectedSeq - 1";
const FIXTURE: &str = "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl";
/// The TypeScript spec checks the fixture's SHA-256; this crate has no hash
/// dependency, so it pins the size and record count.
const FIXTURE_BYTES: usize = 4533;
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 104;
/// Bounds the TypeScript oracle's priming rows.
const MAX_EXPECTED_SEQ: u64 = 16;
const LIMITS: [&str; 9] = [
    "obsolete-seq-diagnostic",
    "system-turn-float-lexeme",
    "system-step-float-lexeme",
    "start-seq-float-lexeme",
    "end-seq-float-lexeme",
    "system-payload",
    "envelope/negative-zero-seq",
    "envelope/seq-diagnostic",
    "envelope/source-output-budget",
];
/// Every refusal kind this crate reports, which the cases must all witness.
const REFUSALS: [&str; 27] = [
    "header-data-not-object",
    "header-not-object",
    "system-data-not-object",
    "missing-field",
    "system-unexpected-fields",
    "invalid-coordinate",
    "coordinate-not-positive",
    "system-message-not-object",
    "system-identity",
    "system-source-not-object",
    "system-source",
    "envelope",
    "event-unexpected-fields",
    "missing-surface-op",
    "surface-op-not-object",
    "inexact-replace",
    "invalid-endpoint",
    "later-endpoint",
    "assistant-sources",
    "empty-sources",
    "empty-header-optional",
    "tool-result-data-not-object",
    "tool-result-message-not-object",
    "non-error-tool-result",
    "retired-header-system",
    "obsolete-type",
    "decoded",
];
const VOCABULARY_LISTS: [&str; 6] = [
    "surfaceTypes",
    "dispositionTypes",
    "nativeTypes",
    "obsoleteTypes",
    "objectPrototypeNames",
    "opaqueTypes",
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
    let text = std::fs::read_to_string(repo_path("conformance/session/v3-row-cases.json"))
        .expect("read v3-row-cases.json");
    serde_json::from_str(&text).expect("parse v3-row-cases.json")
}

/// Checks a TypeScript outcome's exact keys and class; the spec checks its messages.
fn check_outcome(outcome: &Value, context: &str) {
    let fields = object(outcome, context);
    let valid = match fields.get("outcome").and_then(Value::as_str) {
        Some("decoded") => {
            keys(fields) == BTreeSet::from(["outcome", "envelope"])
                && outcome["envelope"].is_object()
        }
        Some("rejected") if outcome["class"] == "TypeError" => {
            keys(fields) == BTreeSet::from(["outcome", "class"])
        }
        Some("rejected") => {
            keys(fields) == BTreeSet::from(["outcome", "class", "message"])
                && [
                    "SessionFormatError",
                    "SessionFormatUnsupportedMigrationError",
                ]
                .iter()
                .any(|class| outcome["class"] == *class)
                && outcome["message"].is_string()
        }
        _ => false,
    };
    assert!(valid, "{context}: invalid outcome {outcome}");
}

/// Checks an override's shape and that it replaces a TypeScript outcome it may replace.
fn check_override(rust: &Value, ts: &Value, context: &str) {
    let fields = object(rust, context);
    let valid = match rust["outcome"].as_str() {
        Some("native-subset") => {
            keys(fields) == BTreeSet::from(["outcome", "limit"])
                && LIMITS.iter().any(|limit| rust["limit"] == *limit)
        }
        Some("rejected-class") => {
            rust["class"] == "SessionFormatError"
                && ts["class"] == "SessionFormatError"
                && (keys(fields) == BTreeSet::from(["outcome", "class"])
                    || keys(fields) == BTreeSet::from(["outcome", "class", "unexpectedKeys"])
                        && rust["unexpectedKeys"]
                            .as_array()
                            .is_some_and(|keys| keys.len() >= 2))
        }
        _ => false,
    };
    assert!(valid, "{context}: invalid rust override {rust}");
    assert!(
        ts["class"] != "TypeError" || rust["outcome"] == "native-subset",
        "{context}: Rust cannot claim a TypeError"
    );
}

fn refusal_kind(result: &Result<V3CodecEvent<'_>, V3RowRefusal>) -> &'static str {
    let rejection = match result {
        Ok(_) => return "decoded",
        Err(V3RowRefusal::Rejected(rejection)) => rejection,
        Err(V3RowRefusal::Unsupported(V3Unsupported::RetiredHeaderSystem)) => {
            return "retired-header-system";
        }
        Err(V3RowRefusal::Unsupported(V3Unsupported::ObsoleteType { .. })) => {
            return "obsolete-type";
        }
        Err(V3RowRefusal::NativeSubset(_)) => return "native-subset",
        Err(V3RowRefusal::ExpectedSeqOutOfRange) => return "caller-error",
    };
    match rejection {
        V3Rejection::Envelope(_) => "envelope",
        V3Rejection::Structural(rejection) => match rejection {
            StructuralRejection::HeaderDataNotObject => "header-data-not-object",
            StructuralRejection::HeaderNotObject => "header-not-object",
            StructuralRejection::SystemDataNotObject => "system-data-not-object",
            StructuralRejection::MissingField { .. } => "missing-field",
            StructuralRejection::UnexpectedFields { .. } => "system-unexpected-fields",
            StructuralRejection::InvalidCoordinate(_) => "invalid-coordinate",
            StructuralRejection::CoordinateNotPositive(_) => "coordinate-not-positive",
            StructuralRejection::SystemMessageNotObject => "system-message-not-object",
            StructuralRejection::SystemIdentity => "system-identity",
            StructuralRejection::SystemSourceNotObject => "system-source-not-object",
            StructuralRejection::SystemSource => "system-source",
        },
        V3Rejection::Event { rejection, .. } => match rejection {
            EventRejection::UnexpectedFields { .. } => "event-unexpected-fields",
            EventRejection::MissingSurfaceOp => "missing-surface-op",
            EventRejection::SurfaceOpNotObject => "surface-op-not-object",
            EventRejection::InexactReplace => "inexact-replace",
            EventRejection::InvalidEndpoint(_) => "invalid-endpoint",
            EventRejection::LaterEndpoint => "later-endpoint",
            EventRejection::AssistantSources => "assistant-sources",
            EventRejection::EmptySources => "empty-sources",
            EventRejection::EmptyHeaderOptional => "empty-header-optional",
            EventRejection::ToolResultDataNotObject => "tool-result-data-not-object",
            EventRejection::ToolResultMessageNotObject => "tool-result-message-not-object",
            EventRejection::NonErrorToolResult => "non-error-tool-result",
        },
    }
}

const fn limit_name(limit: V3Limit) -> &'static str {
    match limit {
        V3Limit::ObsoleteSeqDiagnostic => "obsolete-seq-diagnostic",
        V3Limit::FloatLexeme(V3NumberField::SystemCoordinate(Coordinate::Turn)) => {
            "system-turn-float-lexeme"
        }
        V3Limit::FloatLexeme(V3NumberField::SystemCoordinate(Coordinate::Step)) => {
            "system-step-float-lexeme"
        }
        V3Limit::FloatLexeme(V3NumberField::Endpoint(Endpoint::StartSeq)) => {
            "start-seq-float-lexeme"
        }
        V3Limit::FloatLexeme(V3NumberField::Endpoint(Endpoint::EndSeq)) => "end-seq-float-lexeme",
        V3Limit::SystemPayload => "system-payload",
        V3Limit::Envelope(EnvelopeLimit::NegativeZeroSeq) => "envelope/negative-zero-seq",
        V3Limit::Envelope(EnvelopeLimit::SeqDiagnostic) => "envelope/seq-diagnostic",
        V3Limit::Envelope(EnvelopeLimit::Source(SourceEventSeqsLimit::OutputBudget)) => {
            "envelope/source-output-budget"
        }
        V3Limit::Envelope(EnvelopeLimit::FloatLexeme(_)) => "envelope/float-lexeme",
        V3Limit::Envelope(EnvelopeLimit::Source(SourceEventSeqsLimit::FloatLexeme { .. })) => {
            "envelope/source-float-lexeme"
        }
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

fn outcome_value(result: &Result<V3CodecEvent<'_>, V3RowRefusal>, expected_seq: u64) -> Value {
    match result {
        Ok(event) => json!({"outcome": "decoded", "envelope": envelope_value(event.envelope())}),
        Err(V3RowRefusal::Rejected(rejection)) => match rejection.message(expected_seq) {
            Some(message) => {
                json!({"outcome": "rejected", "class": "SessionFormatError", "message": message})
            }
            None => {
                let unexpected = match rejection {
                    V3Rejection::Envelope(bake_session::EnvelopeRejection::UnexpectedFields {
                        keys,
                    })
                    | V3Rejection::Structural(StructuralRejection::UnexpectedFields {
                        keys, ..
                    })
                    | V3Rejection::Event {
                        rejection: EventRejection::UnexpectedFields { keys },
                        ..
                    } => Some(keys),
                    _ => None,
                };
                match unexpected {
                    Some(keys) => json!({
                        "outcome": "rejected-class",
                        "class": "SessionFormatError",
                        "unexpectedKeys": keys,
                    }),
                    None => json!({"outcome": "rejected-class", "class": "SessionFormatError"}),
                }
            }
        },
        Err(V3RowRefusal::Unsupported(unsupported)) => json!({
            "outcome": "rejected",
            "class": "SessionFormatUnsupportedMigrationError",
            "message": unsupported.message(),
        }),
        Err(V3RowRefusal::NativeSubset(limit)) => {
            json!({"outcome": "native-subset", "limit": limit_name(*limit)})
        }
        Err(V3RowRefusal::ExpectedSeqOutOfRange) => json!({"outcome": "caller-error"}),
    }
}

/// Decodes one row, compares it with the expected outcome, and checks borrowing.
fn check_row(
    row: &Value,
    expected_seq: u64,
    budget: usize,
    expected: &Value,
    context: &str,
) -> &'static str {
    let result = decode_v3_row(row, expected_seq, budget);
    assert_eq!(
        &outcome_value(&result, expected_seq),
        expected,
        "{context}: outcome"
    );
    if let Ok(event) = &result {
        assert_borrowed(event.envelope(), row, context);
    }
    if let Err(V3RowRefusal::NativeSubset(limit)) = &result {
        return limit_name(*limit);
    }
    refusal_kind(&result)
}

fn string_list<'a>(value: &'a Value, context: &str) -> Vec<&'a str> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{context}: expected an array"))
        .iter()
        .map(|name| {
            name.as_str()
                .unwrap_or_else(|| panic!("{context}: {name} is not a string"))
        })
        .collect()
}

#[test]
fn shared_v3_row_cases_match() {
    let table = table();
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from([
            "schema",
            "version",
            "oracle",
            "defaultBudget",
            "vocabulary",
            "fixture",
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
    let mut witnessed = BTreeSet::new();
    let mut limits = BTreeSet::new();
    let mut class_only = BTreeSet::new();
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
                .all(|key| ["id", "expectedSeq", "row", "budget", "ts", "rust"].contains(key)),
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
        let expected = match case.get("rust") {
            Some(rust) => {
                check_override(rust, ts, id);
                if rust["outcome"] == "rejected-class" {
                    class_only.insert(rust.get("unexpectedKeys").is_some());
                }
                rust
            }
            None => ts,
        };
        let row: Value = serde_json::from_str(case["row"].as_str().expect("row is JSON text"))
            .unwrap_or_else(|error| panic!("{id}: row is not one JSON value: {error}"));
        let kind = check_row(
            &row,
            expected_seq,
            usize::try_from(budget).expect("budget fits usize"),
            expected,
            id,
        );
        if expected["outcome"] == "native-subset" {
            limits.insert(kind);
        } else {
            witnessed.insert(kind);
        }
    }
    assert_eq!(
        witnessed,
        BTreeSet::from(REFUSALS),
        "every refusal witnessed"
    );
    // serde_json reads this seq as -0, but JavaScript reads -5e-324.
    let underflow = cases
        .iter()
        .find(|case| case["id"] == "obsolete-underflow-seq")
        .expect("underflow witness");
    assert!(
        underflow["ts"]["message"]
            .as_str()
            .is_some_and(|message| message.ends_with(" at seq -5e-324"))
    );
    assert_eq!(
        underflow["rust"],
        json!({"outcome": "native-subset", "limit": "obsolete-seq-diagnostic"})
    );
    assert_eq!(limits, BTreeSet::from(LIMITS), "every limit witnessed");
    assert_eq!(
        class_only,
        BTreeSet::from([false, true]),
        "both class-only refusals witnessed"
    );
}

#[test]
fn every_listed_type_is_classified_as_the_codec_does() {
    let table = table();
    let vocabulary = object(&table["vocabulary"], "vocabulary");
    assert_eq!(keys(vocabulary), BTreeSet::from(VOCABULARY_LISTS));
    let list = |name: &str| string_list(&vocabulary[name], name);
    let (surface, obsolete, opaque) = (
        list("surfaceTypes"),
        list("obsoleteTypes"),
        list("opaqueTypes"),
    );
    let names: BTreeSet<&str> = VOCABULARY_LISTS
        .iter()
        .flat_map(|name| list(name))
        .collect();
    let system = json!({
        "turn": 1,
        "step": 1,
        "message": {"id": "s", "role": "system", "source": {"kind": "plugin", "plugin": "p"}, "content": []},
    });
    for name in names {
        let data = if name == "system/message" {
            system.clone()
        } else {
            json!({"header": {}})
        };
        let row = json!({"type": name, "seq": 0, "time": 0, "data": data, "ignorable": true, "surfaceOp": 1});
        let subject = format!("format v3 {name} at seq 0");
        let expected = if surface.contains(&name) {
            json!({"outcome": "rejected", "class": "SessionFormatError", "message": format!("{subject} surfaceOp must be an object")})
        } else if obsolete.contains(&name) || opaque.contains(&name) {
            json!({"outcome": "decoded", "envelope": {"type": name, "seq": 0, "time": 0, "ignorable": true, "surfaceOp": 1}})
        } else {
            json!({"outcome": "rejected", "class": "SessionFormatError", "message": format!("{subject} has unexpected field surfaceOp")})
        };
        check_row(&row, 0, 0, &expected, name);
    }
}

#[test]
fn fixture_rows_and_mutants_decode_like_the_codec() {
    let table = table();
    let fixture = object(&table["fixture"], "fixture");
    assert_eq!(
        keys(fixture),
        BTreeSet::from(["log", "events", "types", "mutants"])
    );
    assert_eq!(fixture["log"], FIXTURE);
    let log = std::fs::read_to_string(repo_path(FIXTURE)).expect("read fixture log");
    assert_eq!(log.len(), FIXTURE_BYTES, "fixture size");
    let rows: Vec<Value> = log
        .strip_suffix('\n')
        .expect("final LF")
        .split('\n')
        .skip(1)
        .map(|row| serde_json::from_str(row).expect("fixture row is JSON"))
        .collect();
    assert_eq!(rows.len() as u64, fixture["events"], "fixture events");
    let mut types = BTreeSet::new();
    for (seq, row) in (0_u64..).zip(&rows) {
        let event = decode_v3_row(row, seq, 1024)
            .unwrap_or_else(|refusal| panic!("fixture row {seq}: {refusal:?}"));
        assert_borrowed(event.envelope(), row, &format!("fixture row {seq}"));
        // The codec emits the released v2 envelope unchanged.
        assert_eq!(
            Ok(event.envelope()),
            decode_row_envelope(row, seq, 1024).as_ref()
        );
        types.insert(event.envelope().event_type);
    }
    assert_eq!(types.len() as u64, fixture["types"], "fixture types");

    let mutants = fixture["mutants"].as_array().expect("mutants array");
    assert!(!mutants.is_empty());
    for mutant in mutants {
        let mutant = object(mutant, "mutant");
        let id = mutant["id"].as_str().expect("mutant id");
        let seq = mutant["seq"].as_u64().expect("mutant seq");
        let pointer = mutant["pointer"].as_str().expect("mutant pointer");
        let (parent, member) = pointer.rsplit_once('/').expect("pointer member");
        let mut row = rows[usize::try_from(seq).expect("seq fits usize")].clone();
        row.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .unwrap_or_else(|| panic!("{id}: {parent} is not an object"))
            .insert(member.to_owned(), mutant["value"].clone());
        let ts = &mutant["ts"];
        check_outcome(ts, id);
        let expected = mutant.get("rust").map_or(ts, |rust| {
            check_override(rust, ts, id);
            rust
        });
        check_row(&row, seq, 1024, expected, id);
    }
}
