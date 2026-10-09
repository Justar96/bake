//! `conformance/session/json-parse-cases.json`: `parse_json` and
//! `json_text` against `JSON.stringify(JSON.parse(text))`. The TypeScript
//! arm, `json-parse-conformance.spec.ts` in session-persistence-jsonl, runs
//! the same table.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{JsonParseError, dismantle, json_text, parse_json};
use serde_json::Value;

const SCHEMA: &str = "bake/session-conformance/json-parse-cases";
const ORACLE: &str = "JSON.stringify(JSON.parse(text)), the parse scanLog runs on every record and the serialization a request takes; a thrown SyntaxError is syntax-error";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 50;

fn table() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../conformance/session/json-parse-cases.json");
    let text = std::fs::read_to_string(path).expect("read json-parse-cases.json");
    serde_json::from_str(&text).expect("parse json-parse-cases.json")
}

fn refusal(name: &str) -> JsonParseError {
    match name {
        "lone-surrogate" => JsonParseError::LoneSurrogate,
        "number-out-of-range" => JsonParseError::NumberOutOfRange,
        other => panic!("unknown refusal {other}"),
    }
}

#[test]
fn shared_cases_parse_as_json_parse_does() {
    let table = table();
    let fields = table.as_object().expect("table object");
    assert_eq!(
        fields.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["schema", "version", "history", "oracle", "cases"])
    );
    assert_eq!(fields["schema"], SCHEMA);
    assert_eq!(fields["version"], 1);
    assert_eq!(fields["oracle"], ORACLE);
    assert!(
        fields["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    let cases = fields["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases
        .iter()
        .map(|case| case["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids.len(), CASE_COUNT, "unique ids");
    let mut refusals = BTreeSet::new();
    for case in cases {
        let id = case["id"].as_str().expect("id");
        let text = case["text"].as_str().expect("text");
        let parsed = parse_json(text);
        if let Some(rust) = case.get("rust") {
            let name = rust["refusal"].as_str().expect("refusal name");
            assert_eq!(parsed, Err(refusal(name)), "{id}");
            refusals.insert(name);
            continue;
        }
        match case["ts"]["outcome"].as_str().expect("outcome") {
            "syntax-error" => assert_eq!(parsed, Err(JsonParseError::Syntax), "{id}"),
            "parsed" => {
                let value = parsed.unwrap_or_else(|error| panic!("{id}: {error:?}"));
                assert_eq!(json_text(&value), case["ts"]["text"], "{id}");
                dismantle(value);
            }
            other => panic!("{id}: unknown outcome {other}"),
        }
    }
    assert_eq!(
        refusals,
        BTreeSet::from(["lone-surrogate", "number-out-of-range"]),
        "every refusal is witnessed"
    );
}
