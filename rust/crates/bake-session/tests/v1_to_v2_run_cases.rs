//! Runs every shared case in `conformance/session/v1-to-v2-run-cases.json`
//! through `decode_v0_v1_items`, which must decode it strictly, and then
//! `migrate_v1_to_v2_transformed_items`, on both path platforms. A case's
//! expected outcome is its `rust` native-subset marker when present,
//! otherwise `expect`: what the released v1→v2 migration's header check and
//! transformed stage, fed each decoded event through `transformEvent` and
//! each packed run through `transformRun` and finished, return in
//! TypeScript. The expectations were written from the TypeScript sources;
//! nothing here reads TypeScript output.
//!
//! Each case also runs the expanded path, `decode_v0_v1_rows` and
//! `migrate_v1_to_v2_transformed`, with an event index mapped back to the
//! row that emitted it. It must differ from the run path exactly in the
//! cases flagged `expandedDiverges`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    DecodedV1Items, MigratedV1ToV2, PathPlatform, V1CodecRecovery, V1CodecVersion, V1Item,
    V1ToV2Limit, V1ToV2Location, V1ToV2Refusal, decode_v0_v1_items, decode_v0_v1_rows,
    migrate_v1_to_v2_transformed, migrate_v1_to_v2_transformed_items,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/v1-to-v2-run-cases";
const ORACLE: &str = "sessionFormatV1ToV2.migrateHeader and assertReleasedV2Header over a strict releasedV1SessionFormatCodec decode, then createStage({ sourceKind: 'transformed' }), transformEvent for each emitted event and transformRun for each emitted run into a SessionFormatEventCollector, then finish";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 34;
/// The decoder's source budget; no case comes near it.
const SOURCE_BUDGET: usize = 10_000;
/// The limits `transformRun` reaches when it compares a pending attempt's
/// coordinates or emits it; the event-only limits are witnessed in
/// `v1-to-v2-cases.json`.
const LIMITS: [V1ToV2Limit; 3] = [
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
    header: Value,
    rows: Vec<Value>,
    expect: Expected,
    limit: Option<Expected>,
    expanded_diverges: bool,
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
                .unwrap_or_else(|| panic!("{context}: invalid row index {at}")),
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
    let text = std::fs::read_to_string(repo_path("conformance/session/v1-to-v2-run-cases.json"))
        .expect("read v1-to-v2-run-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse v1-to-v2-run-cases.json");
    let fields = object(&table, "table");
    assert_eq!(
        keys(fields),
        BTreeSet::from(["schema", "version", "oracle", "history", "cases"])
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
    let allowed = BTreeSet::from([
        "id",
        "header",
        "rows",
        "expect",
        "rust",
        "expandedDiverges",
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
            let expanded_diverges = match entry.get("expandedDiverges") {
                None => false,
                Some(Value::Bool(true)) => true,
                Some(other) => {
                    panic!("{id}: expandedDiverges must be true when present, got {other}")
                }
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
                header: parse_text(&entry["header"], &format!("{id} header")),
                rows,
                expect: outcome(&entry["expect"], &id),
                limit,
                expanded_diverges,
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

fn decode_items(entry: &Case, platform: PathPlatform) -> DecodedV1Items {
    decode_v0_v1_items(
        &entry.header,
        &entry.rows,
        V1CodecVersion::V1,
        V1CodecRecovery::Strict,
        platform,
        SOURCE_BUDGET,
    )
    .unwrap_or_else(|refusal| panic!("{platform:?} {}: does not decode: {refusal:?}", entry.id))
}

/// The expanded path's outcome, with an event index mapped to the row that
/// emitted the event.
fn expanded_outcome(
    entry: &Case,
    items: &DecodedV1Items,
    platform: PathPlatform,
) -> Result<MigratedV1ToV2, V1ToV2Refusal> {
    let decoded = decode_v0_v1_rows(
        &entry.header,
        &entry.rows,
        V1CodecVersion::V1,
        V1CodecRecovery::Strict,
        platform,
        SOURCE_BUDGET,
    )
    .unwrap_or_else(|refusal| panic!("{platform:?} {}: does not decode: {refusal:?}", entry.id));
    let rows_of_events: Vec<usize> = items
        .items
        .iter()
        .enumerate()
        .flat_map(|(row, item)| {
            let count = match item {
                V1Item::Event(_) => 1,
                V1Item::AssistantChunkRun(run) => {
                    usize::try_from(run.event_count()).expect("a small run")
                }
            };
            std::iter::repeat_n(row, count)
        })
        .collect();
    let to_row = |location: V1ToV2Location| match location {
        V1ToV2Location::Event(index) => V1ToV2Location::Event(rows_of_events[index]),
        other => other,
    };
    migrate_v1_to_v2_transformed(&decoded).map_err(|refusal| match refusal {
        V1ToV2Refusal::Rejected { location, message } => V1ToV2Refusal::Rejected {
            location: to_row(location),
            message,
        },
        V1ToV2Refusal::NativeSubset { location, limit } => V1ToV2Refusal::NativeSubset {
            location: to_row(location),
            limit,
        },
    })
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
    assert!(
        cases.iter().any(|entry| entry.expanded_diverges),
        "some case diverges from the expanded path"
    );
}

#[test]
fn shared_cases_match_on_both_platforms() {
    let mut failures = Vec::new();
    for entry in load() {
        let expected = entry.limit.as_ref().unwrap_or(&entry.expect);
        for platform in [PathPlatform::Posix, PathPlatform::Win32] {
            let items = decode_items(&entry, platform);
            assert!(
                items
                    .items
                    .iter()
                    .any(|item| matches!(item, V1Item::AssistantChunkRun(_))),
                "{}: every case holds a packed run",
                entry.id
            );
            let actual = migrate_v1_to_v2_transformed_items(&items);
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

#[test]
fn expanded_path_differs_exactly_where_flagged() {
    let mut failures = Vec::new();
    for entry in load() {
        let items = decode_items(&entry, PathPlatform::Posix);
        let run_path = migrate_v1_to_v2_transformed_items(&items);
        let expanded = expanded_outcome(&entry, &items, PathPlatform::Posix);
        if (run_path != expanded) != entry.expanded_diverges {
            failures.push(format!(
                "{}: expandedDiverges is {}, run path {run_path:?}, expanded path {expanded:?}",
                entry.id, entry.expanded_diverges
            ));
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
fn rows_decode_is_items_expanded() {
    for entry in load() {
        let items = decode_items(&entry, PathPlatform::Posix);
        let rows = decode_v0_v1_rows(
            &entry.header,
            &entry.rows,
            V1CodecVersion::V1,
            V1CodecRecovery::Strict,
            PathPlatform::Posix,
            SOURCE_BUDGET,
        )
        .expect("decodes");
        let expanded: Vec<Value> = items
            .items
            .iter()
            .flat_map(|item| match item {
                V1Item::Event(event) => vec![event.clone()],
                V1Item::AssistantChunkRun(run) => run.expand(),
            })
            .collect();
        assert_eq!(rows.events, expanded, "{}", entry.id);
        for (row, item) in items.items.iter().enumerate() {
            let V1Item::AssistantChunkRun(run) = item else {
                continue;
            };
            let events = run.expand();
            assert_eq!(events.len() as u64, run.event_count(), "{}", entry.id);
            assert_eq!(
                events.first().and_then(|event| event["seq"].as_u64()),
                Some(run.first_seq())
            );
            assert_eq!(
                events.last().and_then(|event| event["seq"].as_u64()),
                Some(run.last_seq())
            );
            assert_eq!(
                events.last().and_then(|event| event["time"].as_i64()),
                Some(run.last_time())
            );
            assert_eq!(
                run.stream()["type"],
                entry.rows[row]["type"],
                "{}",
                entry.id
            );
            assert_eq!(
                run.stream()["time0"],
                entry.rows[row]["time0"],
                "{}",
                entry.id
            );
        }
    }
}
