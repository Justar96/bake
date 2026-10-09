//! Runs every shared case in `conformance/session/v1-codec-cases.json`
//! through `decode_v0_v1_rows` on both path platforms. A case's expected
//! outcome is its `rust` native-subset marker when present, otherwise its
//! `win32` outcome on [`PathPlatform::Win32`] when present, otherwise
//! `expect`: what the released v0 or v1 physical codec's decoder, fed every
//! row and finished, returns in TypeScript. The expectations were written
//! from the TypeScript sources; nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    DecodedV1Rows, PathPlatform, V1CodecLimit, V1CodecLocation, V1CodecRecovery, V1CodecRefusal,
    V1CodecVersion, decode_v0_v1_rows,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/v1-codec-cases";
const ORACLE: &str = "releasedV0SessionFormatCodec or releasedV1SessionFormatCodec .createDecoder(header, recovery), decodeRow for each row into a SessionFormatEventCollector, then finish";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 141;
/// The budget a case without `sourceBudget` runs with; TypeScript has none.
const DEFAULT_SOURCE_BUDGET: usize = 10_000;
const LIMITS: [V1CodecLimit; 7] = [
    V1CodecLimit::HeaderFloatLexeme,
    V1CodecLimit::SeqFloatLexeme,
    V1CodecLimit::SeqDiagnostic,
    V1CodecLimit::SourceFloatLexeme,
    V1CodecLimit::PackedFloatLexeme,
    V1CodecLimit::SourceOutputBudget,
    V1CodecLimit::UnsafeJsonInteger,
];

/// What one platform run must return.
#[derive(Debug)]
enum Expected {
    Decoded {
        header: Value,
        events: Vec<Value>,
        inherited_event_count: u64,
    },
    Refused {
        location: V1CodecLocation,
        message: String,
    },
    Limit {
        location: V1CodecLocation,
        limit: V1CodecLimit,
    },
}

struct Case {
    id: String,
    version: V1CodecVersion,
    recovery: V1CodecRecovery,
    header: Value,
    rows: Vec<Value>,
    source_budget: usize,
    expect: Expected,
    win32: Option<Expected>,
    limit: Option<Expected>,
}

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

fn parse_text(value: &Value, context: &str) -> Value {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected JSON text"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{context}: {error}"))
}

fn location(value: &Value, context: &str) -> V1CodecLocation {
    match value {
        Value::String(at) if at == "header" => V1CodecLocation::Header,
        Value::String(at) if at == "finish" => V1CodecLocation::Finish,
        Value::Number(at) => V1CodecLocation::Row(
            at.as_u64()
                .and_then(|row| usize::try_from(row).ok())
                .unwrap_or_else(|| panic!("{context}: invalid row index {at}")),
        ),
        _ => panic!("{context}: invalid location {value}"),
    }
}

fn outcome(value: &Value, context: &str) -> Expected {
    let fields = object(value, context);
    match value["outcome"].as_str() {
        Some("decoded")
            if keys(fields)
                == BTreeSet::from(["outcome", "header", "events", "inheritedEventCount"]) =>
        {
            Expected::Decoded {
                header: value["header"].clone(),
                events: value["events"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{context}: events must be an array"))
                    .clone(),
                inherited_event_count: value["inheritedEventCount"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("{context}: invalid inheritedEventCount")),
            }
        }
        Some("refused") if keys(fields) == BTreeSet::from(["outcome", "at", "message"]) => {
            Expected::Refused {
                location: location(&value["at"], context),
                message: value["message"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{context}: message must be a string"))
                    .to_owned(),
            }
        }
        _ => panic!("{context}: invalid outcome {value}"),
    }
}

fn load() -> Vec<Case> {
    let text = std::fs::read_to_string(repo_path("conformance/session/v1-codec-cases.json"))
        .expect("read v1-codec-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse v1-codec-cases.json");
    let fields = object(&table, "table");
    assert_eq!(
        keys(fields),
        BTreeSet::from(["schema", "version", "oracle", "history", "cases"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 2);
    assert_eq!(table["oracle"], ORACLE);
    assert!(
        table["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    let allowed = BTreeSet::from([
        "id",
        "version",
        "recovery",
        "header",
        "rows",
        "sourceBudget",
        "expect",
        "win32",
        "rust",
        "note",
    ]);
    let cases: Vec<Case> = table["cases"]
        .as_array()
        .expect("cases must be an array")
        .iter()
        .map(|entry| {
            let fields = object(entry, "case");
            let id = entry["id"].as_str().expect("case id").to_owned();
            assert!(keys(fields).is_subset(&allowed), "{id}: unknown keys");
            assert!(
                entry.get("note").is_none_or(Value::is_string),
                "{id}: invalid note"
            );
            let version = match entry["version"].as_u64() {
                Some(0) => V1CodecVersion::V0,
                Some(1) => V1CodecVersion::V1,
                _ => panic!("{id}: version must be 0 or 1"),
            };
            let recovery = match entry["recovery"].as_str() {
                Some("strict") => V1CodecRecovery::Strict,
                Some("recoverable") => V1CodecRecovery::Recoverable,
                _ => panic!("{id}: invalid recovery"),
            };
            let rows = entry["rows"]
                .as_array()
                .unwrap_or_else(|| panic!("{id}: rows must be an array"))
                .iter()
                .enumerate()
                .map(|(index, row)| parse_text(row, &format!("{id} row {index}")))
                .collect();
            let source_budget = match entry.get("sourceBudget") {
                None => DEFAULT_SOURCE_BUDGET,
                Some(budget) => budget
                    .as_u64()
                    .and_then(|budget| usize::try_from(budget).ok())
                    .unwrap_or_else(|| panic!("{id}: invalid sourceBudget")),
            };
            let limit = entry.get("rust").map(|rust| {
                let marker = object(rust, &id);
                assert_eq!(keys(marker), BTreeSet::from(["limit", "at"]), "{id}");
                let name = rust["limit"].as_str().unwrap_or_default();
                let limit = LIMITS
                    .into_iter()
                    .find(|limit| limit.name() == name)
                    .unwrap_or_else(|| panic!("{id}: unlisted limit {name}"));
                Expected::Limit {
                    location: location(&rust["at"], &id),
                    limit,
                }
            });
            Case {
                version,
                recovery,
                header: parse_text(&entry["header"], &format!("{id} header")),
                rows,
                source_budget,
                expect: outcome(&entry["expect"], &id),
                win32: entry.get("win32").map(|value| outcome(value, &id)),
                limit,
                id,
            }
        })
        .collect();
    let ids: BTreeSet<&str> = cases.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len(), "case ids must be unique");
    cases
}

/// The first difference between `actual` and `expected`, if any. Numbers
/// compare as JavaScript doubles, so `-0` differs from `0`. All object members
/// must appear in JavaScript's own-key order, including array-index keys.
fn difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    match (actual, expected) {
        (Value::Number(left), Value::Number(right)) => {
            if left.is_u64() && right.is_u64() || left.is_i64() && right.is_i64() {
                return (left != right).then(|| format!("{path}: {left} != {right}"));
            }
            let bits = |number: &serde_json::Number| number.as_f64().map(f64::to_bits);
            (bits(left) != bits(right)).then(|| format!("{path}: {left} != {right}"))
        }
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                return Some(format!("{path}: length {} != {}", left.len(), right.len()));
            }
            left.iter()
                .zip(right)
                .enumerate()
                .find_map(|(index, (left, right))| {
                    difference(left, right, &format!("{path}[{index}]"))
                })
        }
        (Value::Object(left), Value::Object(right)) => {
            if !left.keys().eq(right.keys()) {
                return Some(format!(
                    "{path}: members {:?} != {:?}",
                    left.keys().collect::<Vec<_>>(),
                    right.keys().collect::<Vec<_>>()
                ));
            }
            right
                .iter()
                .find_map(|(key, right)| difference(&left[key], right, &format!("{path}.{key}")))
        }
        _ => (actual != expected).then(|| format!("{path}: {actual} != {expected}")),
    }
}

fn check(
    id: &str,
    actual: Result<DecodedV1Rows, V1CodecRefusal>,
    expected: &Expected,
) -> Option<String> {
    match (actual, expected) {
        (
            Ok(decoded),
            Expected::Decoded {
                header,
                events,
                inherited_event_count,
            },
        ) => {
            if decoded.inherited_event_count != *inherited_event_count {
                return Some(format!(
                    "{id}: inherited count {} != {inherited_event_count}",
                    decoded.inherited_event_count
                ));
            }
            difference(&decoded.header, header, "header")
                .or_else(|| {
                    difference(
                        &Value::Array(decoded.events),
                        &Value::Array(events.clone()),
                        "events",
                    )
                })
                .map(|difference| format!("{id}: {difference}"))
        }
        (
            Err(V1CodecRefusal::Rejected { location, message }),
            Expected::Refused {
                location: expected_location,
                message: expected_message,
            },
        ) if location == *expected_location && message == *expected_message => None,
        (
            Err(V1CodecRefusal::NativeSubset { location, limit }),
            Expected::Limit {
                location: expected_location,
                limit: expected_limit,
            },
        ) if location == *expected_location && limit == *expected_limit => None,
        (actual, expected) => Some(format!("{id}: got {actual:?}, expected {expected:?}")),
    }
}

#[test]
fn table_pins_its_size_and_limits() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let witnessed: BTreeSet<&str> = cases
        .iter()
        .filter_map(|entry| match &entry.limit {
            Some(Expected::Limit { limit, .. }) => Some(limit.name()),
            _ => None,
        })
        .collect();
    let all: BTreeSet<&str> = LIMITS.iter().map(|limit| limit.name()).collect();
    assert_eq!(witnessed, all, "every limit is witnessed");
    assert!(cases.iter().any(|entry| entry.win32.is_some()));
    for version in [V1CodecVersion::V0, V1CodecVersion::V1] {
        for recovery in [V1CodecRecovery::Strict, V1CodecRecovery::Recoverable] {
            assert!(
                cases
                    .iter()
                    .any(|entry| entry.version == version && entry.recovery == recovery),
                "{version:?} {recovery:?} has a case"
            );
        }
    }
}

#[test]
fn shared_cases_match_on_both_platforms() {
    let mut failures = Vec::new();
    for entry in load() {
        for platform in [PathPlatform::Posix, PathPlatform::Win32] {
            let expected = entry.limit.as_ref().unwrap_or(match platform {
                PathPlatform::Win32 => entry.win32.as_ref().unwrap_or(&entry.expect),
                PathPlatform::Posix => &entry.expect,
            });
            let actual = decode_v0_v1_rows(
                &entry.header,
                &entry.rows,
                entry.version,
                entry.recovery,
                platform,
                entry.source_budget,
            );
            if let Some(failure) = check(&entry.id, actual, expected) {
                failures.push(format!("{platform:?} {failure}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
