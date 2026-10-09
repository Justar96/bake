//! Runs every shared case in `conformance/session/v2-to-v3-cases.json`
//! through `migrate_v2_rows` on both path platforms. A case's expected
//! outcome is its `rust` native-subset marker when present, otherwise its
//! `win32` outcome on [`PathPlatform::Win32`] when present, otherwise
//! `expect`: what the strict released v2 codec feeding the real v2→v3
//! migration chain returned in TypeScript. The comparison covers that stage
//! output only, not the catalog's final transformed-artifact restoration.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    MigratedV2, PathPlatform, V2ToV3Layer, V2ToV3Location, V2ToV3Refusal, migrate_v2_rows,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/v2-to-v3-cases";
const ORACLE: &str = "releasedV2SessionFormatCodec.createDecoder(header, 'strict') feeding createSessionFormatChain([sessionFormatV0ToV1, sessionFormatV1ToV2, sessionFormatV2ToV3]).createStream; decoder.finish, then stream.finish";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 370;
/// The budget a case without `sourceBudget` runs with; TypeScript has none.
const DEFAULT_SOURCE_BUDGET: usize = 10_000;
/// Inherent native limits: a fraction or exponent spelling where TypeScript
/// reads a count or safe integer, this crate's own source budget, and a V8
/// `TypeError` text the chain wraps.
const LIMITS: [&str; 7] = [
    "header-float-lexeme",
    "time-float-lexeme",
    "payload-float-lexeme",
    "source-output-budget",
    "object-prototype-type",
    "content-kind-diagnostic",
    "unsafe-json-integer",
];
const EVERY_FAMILY: &str = "every-source-event-family";
/// The released v2 dispositions and the feedback events the migration also
/// admits. The TypeScript spec checks this case's rows against the real exports.
const FAMILIES: [&str; 53] = [
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/attempt",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/message-delete",
    "feedback/message-put",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "subagent/model-selection-policy",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

/// What one platform run must return.
#[derive(Debug)]
enum Expected {
    Migrated {
        header: Value,
        events: Vec<Value>,
        inherited_event_count: u64,
    },
    Refused {
        location: V2ToV3Location,
        layer: V2ToV3Layer,
        message: String,
    },
    Limit {
        location: V2ToV3Location,
        limit: String,
    },
}

struct Case {
    id: String,
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

fn location(value: &Value, context: &str) -> V2ToV3Location {
    match value {
        Value::String(at) if at == "header" => V2ToV3Location::Header,
        Value::String(at) if at == "finish" => V2ToV3Location::Finish,
        Value::Number(at) => V2ToV3Location::Row(
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
        Some("refused")
            if keys(fields) == BTreeSet::from(["outcome", "layer", "at", "message"]) =>
        {
            Expected::Refused {
                location: location(&value["at"], context),
                layer: match value["layer"].as_str() {
                    Some("codec") => V2ToV3Layer::Codec,
                    Some("migration") => V2ToV3Layer::Migration,
                    _ => panic!("{context}: invalid layer"),
                },
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
    let text = std::fs::read_to_string(repo_path("conformance/session/v2-to-v3-cases.json"))
        .expect("read v2-to-v3-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse v2-to-v3-cases.json");
    let fields = object(&table, "table");
    assert_eq!(
        keys(fields),
        BTreeSet::from(["schema", "version", "oracle", "history", "cases"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 2);
    assert_eq!(table["oracle"], ORACLE);
    let allowed = BTreeSet::from([
        "id",
        "header",
        "rows",
        "sourceBudget",
        "expect",
        "win32",
        "rust",
    ]);
    let cases: Vec<Case> = table["cases"]
        .as_array()
        .expect("cases must be an array")
        .iter()
        .map(|entry| {
            let fields = object(entry, "case");
            let id = entry["id"].as_str().expect("case id").to_owned();
            assert!(keys(fields).is_subset(&allowed), "{id}: unknown keys");
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
                let limit = rust["limit"].as_str().unwrap_or_default().to_owned();
                assert!(
                    LIMITS.contains(&limit.as_str()),
                    "{id}: unlisted limit {limit}"
                );
                Expected::Limit {
                    location: location(&rust["at"], &id),
                    limit,
                }
            });
            Case {
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
            if keys(left) != keys(right) {
                return Some(format!(
                    "{path}: members {:?} != {:?}",
                    keys(left),
                    keys(right)
                ));
            }
            if !left.keys().eq(right.keys()) {
                return Some(format!(
                    "{path}: member order {:?} != {:?}",
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
    actual: Result<MigratedV2, V2ToV3Refusal>,
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
                        &Value::Array(migrated.events.clone()),
                        &Value::Array(events.clone()),
                        "events",
                    )
                })
                .map(|difference| format!("{id}: {difference}"))
        }
        (
            Err(V2ToV3Refusal::Rejected {
                location,
                layer,
                message,
            }),
            Expected::Refused {
                location: expected_location,
                layer: expected_layer,
                message: expected_message,
            },
        ) if location == *expected_location
            && layer == *expected_layer
            && message == *expected_message =>
        {
            None
        }
        (
            Err(V2ToV3Refusal::NativeSubset { location, limit }),
            Expected::Limit {
                location: expected_location,
                limit: expected_limit,
            },
        ) if location == *expected_location && limit == *expected_limit => None,
        (actual, expected) => Some(format!("{id}: got {actual:?}, expected {expected:?}")),
    }
}

#[test]
fn table_pins_its_size_limits_and_families() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let witnessed: BTreeSet<&str> = cases
        .iter()
        .filter_map(|entry| match &entry.limit {
            Some(Expected::Limit { limit, .. }) => Some(limit.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(witnessed, BTreeSet::from(LIMITS));
    let every = cases
        .iter()
        .find(|entry| entry.id == EVERY_FAMILY)
        .expect("every-family case");
    assert!(matches!(every.expect, Expected::Migrated { .. }));
    let types: BTreeSet<&str> = every
        .rows
        .iter()
        .map(|row| row["type"].as_str().expect("row type"))
        .collect();
    assert_eq!(types, BTreeSet::from(FAMILIES));
    assert!(cases.iter().any(|entry| entry.win32.is_some()));
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
            // The inputs are borrowed immutably, so the call cannot change them.
            let actual = migrate_v2_rows(&entry.header, &entry.rows, platform, entry.source_budget);
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
