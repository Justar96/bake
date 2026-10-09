//! Runs every shared case in `conformance/session/generation-header-cases.json`
//! through `read_generation_header_record` under both path platforms. A case's
//! `rust` native-subset limit overrides both platforms; otherwise the expected
//! outcome is the one TypeScript's `stat` produced on that platform.

use std::path::PathBuf;

use bake_session::{
    GenerationHeaderRefusal, HeaderOrigin, PathPlatform, SessionHeader, SubsetLimit,
    read_generation_header_record,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-format-conformance/generation-header-cases";
const CASE_COUNT: usize = 98;

fn table() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../conformance/session/generation-header-cases.json");
    let text = std::fs::read_to_string(path).expect("read generation-header-cases.json");
    // Expected headers and messages may hold a lone surrogate, which only
    // `parse_json` reads.
    bake_session::parse_json(&text).expect("parse generation-header-cases.json")
}

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex length");
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex byte"))
        .collect()
}

fn record(case: &Map<String, Value>) -> Vec<u8> {
    match (case.get("record"), case.get("bytesHex")) {
        (Some(Value::String(text)), None) => format!("{text}\n").into_bytes(),
        (None, Some(Value::String(bytes))) => hex(bytes),
        _ => panic!("{}: exactly one record source", case["id"]),
    }
}

fn header(expected: &Map<String, Value>) -> SessionHeader {
    let string = |key: &str| {
        expected
            .get(key)
            .map(|value| value.as_str().expect(key).to_owned())
    };
    assert_eq!(expected["version"], 3);
    SessionHeader {
        id: string("id").expect("id"),
        created_at: expected["createdAt"].as_u64().expect("createdAt"),
        cwd: string("cwd"),
        parent_session: string("parentSession"),
        is_seeded: expected["isSeeded"].as_bool().expect("isSeeded"),
        origin: expected.get("origin").map(|origin| {
            assert_eq!(origin, "subagent");
            HeaderOrigin::Subagent
        }),
        delegation_depth: expected["delegationDepth"]
            .as_u64()
            .expect("delegationDepth"),
        agent_preset: string("agentPreset"),
    }
}

fn limit(name: &str) -> SubsetLimit {
    match name {
        "invalid-utf8" => SubsetLimit::InvalidUtf8,
        "json-parser" => SubsetLimit::JsonParser,
        "float-lexeme" => SubsetLimit::FloatLexeme,
        "version-diagnostic" => SubsetLimit::VersionDiagnostic,
        other => panic!("unknown limit {other}"),
    }
}

type Outcome = Result<Option<SessionHeader>, GenerationHeaderRefusal>;

fn outcome(expected: &Value) -> Outcome {
    let expected = expected.as_object().expect("outcome object");
    let message = || expected["message"].as_str().expect("message").to_owned();
    match expected["outcome"].as_str() {
        Some("absent") => Ok(None),
        Some("header") => Ok(Some(header(
            expected["header"].as_object().expect("header"),
        ))),
        Some("rejected") => Err(GenerationHeaderRefusal::Rejected(message())),
        Some("unsupported") => Err(GenerationHeaderRefusal::Unsupported(message())),
        other => panic!("unknown outcome {other:?}"),
    }
}

#[test]
fn every_shared_generation_header_case_matches() {
    let table = table();
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 2);
    let cases = table["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), CASE_COUNT);
    let mut ids = std::collections::BTreeSet::new();
    for case in cases {
        let case = case.as_object().expect("case object");
        let id = case["id"].as_str().expect("id");
        assert!(ids.insert(id), "duplicate case {id}");
        let source_version = case["sourceVersion"].as_u64().expect("sourceVersion");
        let bytes = record(case);
        for (platform, key) in [
            (PathPlatform::Posix, "expect"),
            (PathPlatform::Win32, "win32"),
        ] {
            let expected = match case.get("rust") {
                Some(rust) => Err(GenerationHeaderRefusal::NativeSubset(limit(
                    rust["limit"].as_str().expect("limit"),
                ))),
                None => outcome(case.get(key).unwrap_or(&case["expect"])),
            };
            assert_eq!(
                read_generation_header_record(&bytes, source_version, platform),
                expected,
                "{id} {platform:?}"
            );
        }
    }
}

#[test]
fn a_record_with_more_than_its_terminating_line_feed_is_absent() {
    let line = br#"{"type":"session","version":3,"id":"s1","createdAt":1,"isSeeded":false,"delegationDepth":0}"#;
    let mut two = line.to_vec();
    two.extend_from_slice(b"\n\n");
    assert_eq!(
        read_generation_header_record(&two, 3, PathPlatform::Posix),
        Ok(None)
    );
    let mut one = line.to_vec();
    one.push(b'\n');
    assert!(matches!(
        read_generation_header_record(&one, 3, PathPlatform::Posix),
        Ok(Some(_))
    ));
}
