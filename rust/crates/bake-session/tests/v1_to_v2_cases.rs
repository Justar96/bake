//! Runs every shared case in `conformance/session/v1-to-v2-cases.json`
//! through `decode_v0_v1_rows`, which must decode it strictly, and then
//! `migrate_v1_to_v2_transformed`, on both path platforms. A case's expected
//! outcome is its `rust` native-subset marker when present, otherwise
//! `expect`: what the released v1→v2 migration's header check and
//! transformed stage, fed every decoded event and finished, return in
//! TypeScript. The expectations were written from the TypeScript sources;
//! nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    MigratedV1ToV2, PathPlatform, V1CodecRecovery, V1CodecVersion, V1ToV2Limit, V1ToV2Location,
    V1ToV2Refusal, decode_v0_v1_rows, migrate_v1_to_v2_transformed,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/v1-to-v2-cases";
const ORACLE: &str = "sessionFormatV1ToV2.migrateHeader and assertReleasedV2Header over a strict releasedV0SessionFormatCodec or releasedV1SessionFormatCodec decode, then createStage({ sourceKind: 'transformed' }), transformEvent for each decoded event into a SessionFormatEventCollector, then finish";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 107;
/// The decoder's source budget; no case comes near it.
const SOURCE_BUDGET: usize = 10_000;
const LIMITS: [V1ToV2Limit; 5] = [
    V1ToV2Limit::AssistantChunk,
    V1ToV2Limit::NonStringType,
    V1ToV2Limit::UncheckedShape,
    V1ToV2Limit::FloatLexeme,
    V1ToV2Limit::UndefinedMember,
];

/// What a run must return.
#[derive(Debug)]
enum Expected {
    Migrated {
        header: Value,
        events: Vec<Value>,
        inherited_event_count: u64,
    },
    Refused {
        location: V1ToV2Location,
        message: String,
    },
    Limit {
        location: V1ToV2Location,
        limit: V1ToV2Limit,
    },
}

struct Case {
    id: String,
    version: V1CodecVersion,
    header: Value,
    rows: Vec<Value>,
    expect: Expected,
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

fn location(value: &Value, context: &str) -> V1ToV2Location {
    match value {
        Value::String(at) if at == "header" => V1ToV2Location::Header,
        Value::String(at) if at == "finish" => V1ToV2Location::Finish,
        Value::Number(at) => V1ToV2Location::Event(
            at.as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or_else(|| panic!("{context}: invalid event index {at}")),
        ),
        _ => panic!("{context}: invalid location {value}"),
    }
}

fn outcome(value: &Value, context: &str) -> Expected {
    let fields = object(value, context);
    match value["outcome"].as_str() {
        Some("migrated")
            if keys(fields)
                == BTreeSet::from(["outcome", "header", "events", "inheritedEventCount"]) =>
        {
            Expected::Migrated {
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
    let text = std::fs::read_to_string(repo_path("conformance/session/v1-to-v2-cases.json"))
        .expect("read v1-to-v2-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse v1-to-v2-cases.json");
    let fields = object(&table, "table");
    assert_eq!(
        keys(fields),
        BTreeSet::from([
            "schema",
            "version",
            "oracle",
            "history",
            "vocabulary",
            "cases"
        ])
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
    let allowed = BTreeSet::from(["id", "version", "header", "rows", "expect", "rust", "note"]);
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
            let rows = entry["rows"]
                .as_array()
                .unwrap_or_else(|| panic!("{id}: rows must be an array"))
                .iter()
                .enumerate()
                .map(|(index, row)| parse_text(row, &format!("{id} row {index}")))
                .collect();
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
                header: parse_text(&entry["header"], &format!("{id} header")),
                rows,
                expect: outcome(&entry["expect"], &id),
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
    actual: Result<MigratedV1ToV2, V1ToV2Refusal>,
    expected: &Expected,
) -> Option<String> {
    match (actual, expected) {
        (
            Ok(migrated),
            Expected::Migrated {
                header,
                events,
                inherited_event_count,
            },
        ) => {
            if migrated.inherited_event_count != *inherited_event_count {
                return Some(format!(
                    "{id}: inherited count {} != {inherited_event_count}",
                    migrated.inherited_event_count
                ));
            }
            difference(&migrated.header, header, "header")
                .or_else(|| {
                    difference(
                        &Value::Array(migrated.events),
                        &Value::Array(events.clone()),
                        "events",
                    )
                })
                .map(|difference| format!("{id}: {difference}"))
        }
        (
            Err(V1ToV2Refusal::Rejected { location, message }),
            Expected::Refused {
                location: expected_location,
                message: expected_message,
            },
        ) if location == *expected_location && message == *expected_message => None,
        (
            Err(V1ToV2Refusal::NativeSubset { location, limit }),
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
}

#[test]
fn shared_cases_match_on_both_platforms() {
    let mut failures = Vec::new();
    for entry in load() {
        let expected = entry.limit.as_ref().unwrap_or(&entry.expect);
        for platform in [PathPlatform::Posix, PathPlatform::Win32] {
            let decoded = match decode_v0_v1_rows(
                &entry.header,
                &entry.rows,
                entry.version,
                V1CodecRecovery::Strict,
                platform,
                SOURCE_BUDGET,
            ) {
                Ok(decoded) => decoded,
                Err(refusal) => {
                    failures.push(format!(
                        "{platform:?} {}: does not decode: {refusal:?}",
                        entry.id
                    ));
                    continue;
                }
            };
            let actual = migrate_v1_to_v2_transformed(&decoded);
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
