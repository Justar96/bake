//! Runs every shared case in `conformance/session/number-cases.json`. A
//! printing case parses its lexeme with serde_json and requires
//! `json_number_text` and `json_text` to write the hand-written
//! `JSON.stringify` text. A derivation case's log must derive, through both
//! `replay_requests` and `restore_plain_log` with `replay_restored_requests`,
//! requests whose `json_text` equals the expected bytes; a case with a `rust`
//! marker must instead be refused by both with the number limit at the seq it
//! names. Both path platforms are run. Nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    PathPlatform, ReplayLimit, ReplayRefusal, RestoreLimit, RestoreRefusal, json_number_text,
    json_text, replay_requests, replay_restored_requests, restore_plain_log,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/number-cases";
const ORACLE: &str = "JSON.stringify(JSON.parse(lexeme)) for printing; for derivation, JSON.stringify of each request replayRequests(log) returns in packages/core/agent-loop/tests/runtime-fixture.ts and of each request number-conformance.spec.ts derives from the Session restorePlainLog(log) restores, as restored-request-derivation-conformance.spec.ts derives one";
/// Both harnesses pin the table sizes, so a dropped case fails.
const PRINTING_COUNT: usize = 56;
const DERIVATION_COUNT: usize = 5;
const SOURCE_BUDGET: usize = 64;

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn object<'a>(value: &'a Value, context: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected an object"))
}

fn keys(fields: &Map<String, Value>) -> BTreeSet<&str> {
    fields.keys().map(String::as_str).collect()
}

fn text<'a>(value: &'a Value, context: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a string"))
}

fn texts<'a>(value: &'a Value, context: &str) -> Vec<&'a str> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{context}: expected an array"))
        .iter()
        .map(|item| text(item, context))
        .collect()
}

fn load() -> Map<String, Value> {
    let path = repo_path("conformance/session/number-cases.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    let table: Value = serde_json::from_slice(&bytes).expect("number-cases.json is JSON");
    let table = object(&table, "table").clone();
    assert_eq!(
        keys(&table),
        BTreeSet::from([
            "derivation",
            "history",
            "oracle",
            "printing",
            "schema",
            "version"
        ])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    assert!(
        texts(&table["history"], "history")
            .iter()
            .all(|line| !line.contains('\n'))
    );
    table
}

#[test]
fn numbers_print_as_json_stringify_writes_them() {
    let table = load();
    let cases = table["printing"].as_array().expect("printing cases");
    assert_eq!(cases.len(), PRINTING_COUNT);
    let mut lexemes = BTreeSet::new();
    for case in cases {
        let case = object(case, "printing case");
        assert_eq!(keys(case), BTreeSet::from(["lexeme", "text"]));
        let lexeme = text(&case["lexeme"], "lexeme");
        let expected = text(&case["text"], lexeme);
        assert!(lexemes.insert(lexeme), "{lexeme}: repeated lexeme");
        let value: Value =
            serde_json::from_str(lexeme).unwrap_or_else(|error| panic!("{lexeme}: {error}"));
        let number = value
            .as_f64()
            .unwrap_or_else(|| panic!("{lexeme}: not a number"));
        assert_eq!(json_number_text(number), expected, "{lexeme}");
        assert_eq!(json_text(&value), expected, "{lexeme}");
    }
}

#[test]
fn derivation_cases_print_requests_as_typescript_does() {
    let table = load();
    let cases = table["derivation"].as_array().expect("derivation cases");
    assert_eq!(cases.len(), DERIVATION_COUNT);
    let mut ids = BTreeSet::new();
    let mut witnessed = false;
    for case in cases {
        let case = object(case, "derivation case");
        let id = text(&case["id"], "id");
        assert!(ids.insert(id), "{id}: repeated id");
        let allowed = BTreeSet::from(["id", "lines", "requests", "rust", "note"]);
        assert!(keys(case).is_subset(&allowed), "{id}: unknown keys");
        assert!(case.get("note").is_none_or(Value::is_string), "{id}: note");
        let log: String = texts(&case["lines"], id)
            .iter()
            .map(|line| {
                assert!(!line.contains('\n'), "{id}: a line holds LF");
                format!("{line}\n")
            })
            .collect();
        let expected = texts(&case["requests"], id);
        for platform in [PathPlatform::Posix, PathPlatform::Win32] {
            let replayed = replay_requests(log.as_bytes(), platform, SOURCE_BUDGET);
            let restored = restore_plain_log(log.as_bytes(), platform, SOURCE_BUDGET);
            if let Some(rust) = case.get("rust") {
                let rust = object(rust, id);
                assert_eq!(keys(rust), BTreeSet::from(["limit", "seq"]), "{id}");
                assert_eq!(rust["limit"], "number", "{id}");
                let seq = rust["seq"].as_u64().expect("limit seq");
                assert_eq!(
                    replayed.err(),
                    Some(ReplayRefusal::NativeSubset {
                        seq,
                        limit: ReplayLimit::Number
                    }),
                    "{id}"
                );
                assert_eq!(
                    restored.err(),
                    Some(RestoreRefusal::NativeSubset {
                        seq,
                        limit: RestoreLimit::Number
                    }),
                    "{id}"
                );
                witnessed = true;
                continue;
            }
            let print = |request: &bake_session::Request| json_text(&request.to_json());
            let replayed: Vec<String> = replayed
                .unwrap_or_else(|refusal| panic!("{id}: replay refused {refusal:?}"))
                .iter()
                .map(print)
                .collect();
            assert_eq!(replayed, expected, "{id}: replay_requests");
            let restored =
                restored.unwrap_or_else(|refusal| panic!("{id}: restore refused {refusal:?}"));
            assert!(restored.torn().is_none(), "{id}: torn tail");
            let derived: Vec<String> = replay_restored_requests(&restored)
                .unwrap_or_else(|refusal| panic!("{id}: derivation refused {refusal:?}"))
                .iter()
                .map(print)
                .collect();
            assert_eq!(derived, expected, "{id}: replay_restored_requests");
        }
    }
    assert!(witnessed, "the number limit is witnessed");
}
