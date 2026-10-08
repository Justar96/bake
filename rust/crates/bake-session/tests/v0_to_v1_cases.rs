//! Runs every shared case in `conformance/session/v0-to-v1-cases.json`
//! through `decode_v0_v1_rows` at version 0, which must succeed, then
//! `migrate_v0_to_v1`. A case's expected outcome is its `rust` native-subset
//! marker when present, otherwise `expect`: what the released v0 codec's
//! decoder feeding the v0→v1 migration chain returns in TypeScript. The
//! expectations were written from the TypeScript sources; nothing here reads
//! TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    MigratedV1, PathPlatform, V0ToV1Location, V0ToV1Refusal, V1CodecRecovery, V1CodecVersion,
    decode_v0_v1_rows, migrate_v0_to_v1,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/v0-to-v1-cases";
const ORACLE: &str = "releasedV0SessionFormatCodec.createDecoder(header, recovery) feeding createSessionFormatChain({currentVersion: 1, migrations: [sessionFormatV0ToV1]}).createStream; decoder.finish, then stream.finish";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 95;
/// No case reaches the codec's own source budget.
const SOURCE_BUDGET: usize = 10_000;
const LIMITS: [&str; 6] = [
    "seq-float-lexeme",
    "type-coercion",
    "object-prototype-type",
    "payload-float-lexeme",
    "reference-float-lexeme",
    "legacy-goal-message",
];

#[derive(Debug)]
enum Expected {
    Migrated {
        header: Value,
        events: Vec<Value>,
        inherited_event_count: u64,
    },
    Refused {
        location: V0ToV1Location,
        message: String,
    },
    Limit {
        location: V0ToV1Location,
        limit: String,
    },
}

struct Case {
    id: String,
    recovery: V1CodecRecovery,
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

fn location(value: &Value, context: &str) -> V0ToV1Location {
    match value {
        Value::String(at) if at == "header" => V0ToV1Location::Header,
        Value::Number(at) => V0ToV1Location::Event(
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
    let text = std::fs::read_to_string(repo_path("conformance/session/v0-to-v1-cases.json"))
        .expect("read v0-to-v1-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse v0-to-v1-cases.json");
    let fields = object(&table, "table");
    assert_eq!(
        keys(fields),
        BTreeSet::from([
            "schema",
            "version",
            "oracle",
            "history",
            "dispositions",
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
    let allowed = BTreeSet::from(["id", "recovery", "header", "rows", "expect", "rust", "note"]);
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
            let limit = entry.get("rust").map(|rust| {
                let marker = object(rust, &id);
                assert_eq!(keys(marker), BTreeSet::from(["limit", "at"]), "{id}");
                let name = rust["limit"].as_str().unwrap_or_default();
                assert!(LIMITS.contains(&name), "{id}: unlisted limit {name}");
                Expected::Limit {
                    location: location(&rust["at"], &id),
                    limit: name.to_owned(),
                }
            });
            Case {
                recovery,
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
/// must appear in JavaScript's own-key order.
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
    actual: Result<MigratedV1, V0ToV1Refusal>,
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
            Err(V0ToV1Refusal::Rejected { location, message }),
            Expected::Refused {
                location: expected_location,
                message: expected_message,
            },
        ) if location == *expected_location && message == *expected_message => None,
        (
            Err(V0ToV1Refusal::NativeSubset { location, limit }),
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
            Some(Expected::Limit { limit, .. }) => Some(limit.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        witnessed,
        BTreeSet::from(LIMITS),
        "every limit is witnessed"
    );
    assert!(
        cases
            .iter()
            .any(|entry| entry.recovery == V1CodecRecovery::Recoverable)
    );
}

#[test]
fn shared_cases_match() {
    let mut failures = Vec::new();
    for entry in load() {
        let expected = entry.limit.as_ref().unwrap_or(&entry.expect);
        let decoded = decode_v0_v1_rows(
            &entry.header,
            &entry.rows,
            V1CodecVersion::V0,
            entry.recovery,
            PathPlatform::Posix,
            SOURCE_BUDGET,
        );
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(refusal) => {
                failures.push(format!("{}: decode failed: {refusal:?}", entry.id));
                continue;
            }
        };
        let before = decoded.clone();
        let actual = migrate_v0_to_v1(&decoded);
        assert_eq!(decoded, before, "{}: input changed", entry.id);
        if let Some(failure) = check(&entry.id, actual, expected) {
            failures.push(failure);
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_v1_header_is_a_decode_invariant_limit() {
    let header: Value = serde_json::from_str(
        r#"{"type":"session","version":1,"id":"v1","createdAt":0,"delegationDepth":0}"#,
    )
    .expect("header");
    let decoded = decode_v0_v1_rows(
        &header,
        &[],
        V1CodecVersion::V1,
        V1CodecRecovery::Strict,
        PathPlatform::Posix,
        SOURCE_BUDGET,
    )
    .expect("decode");
    assert_eq!(
        migrate_v0_to_v1(&decoded),
        Err(V0ToV1Refusal::NativeSubset {
            location: V0ToV1Location::Header,
            limit: "decode-invariant".to_owned(),
        })
    );
}
