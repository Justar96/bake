//! Runs every shared case in `conformance/session/relationships-cases.json`
//! through `check_released_relationships`. A case's expected outcome is its
//! `rust` native-subset marker when present, otherwise `ts`: what
//! `assertReleasedArtifactRelationships` returns or throws in TypeScript, and
//! the index of the event it was checking. A case marked
//! `outsidePrecondition` breaks the precondition the function documents;
//! Rust still decides it unless a marker says otherwise. The expectations
//! were written from the TypeScript sources; nothing here reads TypeScript
//! output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{RelationshipExtensions, RelationshipRefusal, check_released_relationships};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/relationships-cases";
const ORACLE: &str = "assertReleasedArtifactRelationships({header, inheritedEventCount, events}, extensions) from session-format-v0-to-v1";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 182;
const LIMITS: [&str; 2] = ["precondition", "prototype-member"];

#[derive(Debug, PartialEq, Eq)]
enum Expected {
    Accepted,
    Rejected { at: u64, message: String },
    Limit { at: u64, limit: String },
}

struct Case {
    id: String,
    header: Value,
    inherited_event_count: u64,
    extensions: RelationshipExtensions,
    events: Vec<Value>,
    expect: Expected,
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn object<'a>(value: &'a Value, context: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected an object, got {value}"))
}

fn keys(fields: &Map<String, Value>) -> BTreeSet<&str> {
    fields.keys().map(String::as_str).collect()
}

fn text<'a>(value: &'a Value, context: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a string"))
}

fn parse_text(value: &Value, context: &str) -> Value {
    serde_json::from_str(text(value, context)).unwrap_or_else(|error| panic!("{context}: {error}"))
}

fn index(value: &Value, context: &str) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("{context}: expected an index"))
}

fn extensions(value: Option<&Value>, id: &str) -> RelationshipExtensions {
    let Some(value) = value else {
        return RelationshipExtensions::default();
    };
    let fields = object(value, id);
    assert!(
        keys(fields).is_subset(&BTreeSet::from([
            "stepEvents",
            "preservedSourceTitleRequestText",
            "legacyInterruptedTurnRestart",
        ])),
        "{id}: unknown extensions"
    );
    let flag = |key: &str| match fields.get(key) {
        None => false,
        Some(Value::Bool(true)) => true,
        Some(_) => panic!("{id}: {key} must be true when present"),
    };
    RelationshipExtensions {
        step_events: fields.get("stepEvents").map_or_else(Vec::new, |types| {
            types
                .as_array()
                .unwrap_or_else(|| panic!("{id}: stepEvents must be an array"))
                .iter()
                .map(|step_event| text(step_event, id).to_owned())
                .collect()
        }),
        preserved_source_title_request_text: flag("preservedSourceTitleRequestText"),
        legacy_interrupted_turn_restart: flag("legacyInterruptedTurnRestart"),
    }
}

fn ts_outcome(value: &Value, id: &str) -> Option<Expected> {
    let fields = object(value, id);
    match text(&fields["outcome"], id) {
        "accepted" => {
            assert_eq!(keys(fields), BTreeSet::from(["outcome"]), "{id}");
            Some(Expected::Accepted)
        }
        "rejected" => {
            assert_eq!(
                keys(fields),
                BTreeSet::from(["outcome", "at", "message"]),
                "{id}"
            );
            Some(Expected::Rejected {
                at: index(&fields["at"], id),
                message: text(&fields["message"], id).to_owned(),
            })
        }
        "threw" => {
            assert_eq!(keys(fields), BTreeSet::from(["outcome", "error"]), "{id}");
            assert_eq!(fields["error"], "TypeError", "{id}");
            None
        }
        other => panic!("{id}: invalid ts outcome {other}"),
    }
}

fn load() -> Vec<Case> {
    let text_value =
        std::fs::read_to_string(repo_path("conformance/session/relationships-cases.json"))
            .expect("read relationships-cases.json");
    let table: Value = serde_json::from_str(&text_value).expect("parse relationships-cases.json");
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
        "inheritedEventCount",
        "extensions",
        "events",
        "outsidePrecondition",
        "ts",
        "rust",
        "note",
    ]);
    table["cases"]
        .as_array()
        .expect("cases must be an array")
        .iter()
        .map(|entry| {
            let fields = object(entry, "case");
            let id = text(&entry["id"], "case id").to_owned();
            assert!(keys(fields).is_subset(&allowed), "{id}: unknown keys");
            assert!(
                entry.get("note").is_none_or(Value::is_string),
                "{id}: invalid note"
            );
            let outside = match entry.get("outsidePrecondition") {
                None => false,
                Some(Value::Bool(true)) => true,
                Some(_) => panic!("{id}: outsidePrecondition must be true when present"),
            };
            let ts = ts_outcome(&entry["ts"], &id);
            let expect = match entry.get("rust") {
                None => ts.unwrap_or_else(|| panic!("{id}: a TypeError needs a Rust limit")),
                Some(rust) => {
                    let rust = object(rust, &id);
                    assert_eq!(
                        keys(rust),
                        BTreeSet::from(["outcome", "limit", "at"]),
                        "{id}"
                    );
                    assert_eq!(rust["outcome"], "native-subset", "{id}");
                    let limit = text(&rust["limit"], &id);
                    assert!(LIMITS.contains(&limit), "{id}: unlisted limit {limit}");
                    assert!(
                        limit != "precondition" || outside,
                        "{id}: a precondition limit needs outsidePrecondition"
                    );
                    Expected::Limit {
                        at: index(&rust["at"], &id),
                        limit: limit.to_owned(),
                    }
                }
            };
            Case {
                header: parse_text(&entry["header"], &format!("{id} header")),
                inherited_event_count: index(&entry["inheritedEventCount"], &id),
                extensions: extensions(entry.get("extensions"), &id),
                events: entry["events"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{id}: events must be an array"))
                    .iter()
                    .enumerate()
                    .map(|(position, row)| parse_text(row, &format!("{id} event {position}")))
                    .collect(),
                expect,
                id,
            }
        })
        .collect()
}

fn observe(case: &Case) -> Expected {
    match check_released_relationships(
        &case.header,
        case.inherited_event_count,
        &case.events,
        &case.extensions,
    ) {
        Ok(()) => Expected::Accepted,
        Err(RelationshipRefusal::Rejected { seq, message }) => {
            Expected::Rejected { at: seq, message }
        }
        Err(RelationshipRefusal::NativeSubset { seq, limit }) => Expected::Limit {
            at: seq,
            limit: limit.name().to_owned(),
        },
    }
}

#[test]
fn shared_relationship_cases_match_typescript() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len(), "case ids must be unique");
    let mut witnessed = BTreeSet::new();
    let mut failures = Vec::new();
    for case in &cases {
        if let Expected::Limit { limit, .. } = &case.expect {
            witnessed.insert(limit.clone());
        }
        let actual = observe(case);
        if actual != case.expect {
            failures.push(format!(
                "{}: expected {:?}, got {actual:?}",
                case.id, case.expect
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(
        witnessed,
        LIMITS.iter().map(|limit| (*limit).to_owned()).collect()
    );
}
