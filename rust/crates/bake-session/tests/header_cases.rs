//! Runs every shared Session header case in `conformance/session/header-cases.json`
//! through `read_header_record` under both path platforms. A case's expected
//! outcome is its `rust` native-subset override when present, otherwise the
//! TypeScript outcome the real scanner produced on that platform.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    HeaderOrigin, HeaderRefusal, PathPlatform, Rejection, SessionHeader, SubsetLimit, first_record,
    read_header_record,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-format-conformance/header-cases";
const ORACLE: &str = "new SessionLogScanner(record, 'strict') from packages/session/session-persistence-jsonl/src/format.ts";
/// The only log a case may take its first record from.
const FIXTURE: &str = "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl";
const SOURCES: [&str; 3] = ["record", "bytesHex", "fixtureFirstRecord"];
const META_REQUIRED: [&str; 5] = ["version", "id", "createdAt", "isSeeded", "delegationDepth"];
const META_OPTIONAL: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];

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

/// Checks one `{outcome, ...}` expectation's exact keys and vocabulary.
fn check_outcome(value: &Value, context: &str) {
    let outcome = object(value, context);
    let valid = match outcome.get("outcome").and_then(Value::as_str) {
        // `type-error`: the scanner threw a TypeError, a class Rust never claims.
        Some("admitted" | "type-error") => keys(outcome) == BTreeSet::from(["outcome"]),
        Some("unsupported") => {
            keys(outcome) == BTreeSet::from(["outcome", "newer"]) && outcome["newer"].is_boolean()
        }
        Some("rejected") => {
            keys(outcome) == BTreeSet::from(["outcome", "reason"])
                && [
                    "framing",
                    "json",
                    "not-object",
                    "retired-policy-fields",
                    "not-session-header",
                ]
                .iter()
                .any(|reason| outcome["reason"] == *reason)
        }
        _ => false,
    };
    assert!(valid, "{context}: invalid outcome {value}");
}

/// The TypeScript outcome for one platform, after validating the `ts` field.
fn ts_outcome<'a>(ts: &'a Value, platform: PathPlatform, id: &str) -> &'a Value {
    let ts_object = object(ts, id);
    if ts_object.contains_key("outcome") {
        check_outcome(ts, id);
        return ts;
    }
    assert_eq!(
        keys(ts_object),
        BTreeSet::from(["posix", "win32"]),
        "{id}: ts keys"
    );
    check_outcome(&ts["posix"], id);
    check_outcome(&ts["win32"], id);
    match platform {
        PathPlatform::Posix => &ts["posix"],
        PathPlatform::Win32 => &ts["win32"],
    }
}

fn check_meta(meta: &Value, id: &str) {
    let meta = object(meta, id);
    let present = keys(meta);
    assert!(
        META_REQUIRED.iter().all(|key| present.contains(key))
            && present
                .iter()
                .all(|key| META_REQUIRED.contains(key) || META_OPTIONAL.contains(key)),
        "{id}: meta keys {present:?}"
    );
}

fn decode_hex(hex: &str, id: &str) -> Vec<u8> {
    assert!(
        hex.len().is_multiple_of(2) && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{id}: bytesHex must be lowercase hex"
    );
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn record_bytes(case: &Map<String, Value>, id: &str) -> Vec<u8> {
    let sources: Vec<&str> = SOURCES
        .into_iter()
        .filter(|key| case.contains_key(*key))
        .collect();
    assert_eq!(sources.len(), 1, "{id}: exactly one record source");
    let source = case[sources[0]]
        .as_str()
        .unwrap_or_else(|| panic!("{id}: source must be a string"));
    match sources[0] {
        "record" => format!("{source}\n").into_bytes(),
        "bytesHex" => decode_hex(source, id),
        _ => {
            assert_eq!(source, FIXTURE, "{id}: unsupported fixture path");
            let log = std::fs::read(repo_path(FIXTURE)).expect("read fixture log");
            first_record(&log)
                .expect("fixture has a header record")
                .to_vec()
        }
    }
}

fn header_meta(header: &SessionHeader) -> Value {
    let mut meta = json!({
        "version": 3,
        "id": header.id,
        "createdAt": header.created_at,
        "isSeeded": header.is_seeded,
        "delegationDepth": header.delegation_depth,
    });
    let fields = meta.as_object_mut().expect("meta object");
    if let Some(cwd) = &header.cwd {
        fields.insert("cwd".into(), json!(cwd));
    }
    if let Some(parent) = &header.parent_session {
        fields.insert("parentSession".into(), json!(parent));
    }
    if let Some(HeaderOrigin::Subagent) = header.origin {
        fields.insert("origin".into(), json!("subagent"));
    }
    if let Some(preset) = &header.agent_preset {
        fields.insert("agentPreset".into(), json!(preset));
    }
    meta
}

fn outcome_value(result: &Result<SessionHeader, HeaderRefusal>) -> Value {
    match result {
        Ok(_) => json!({"outcome": "admitted"}),
        Err(HeaderRefusal::UnsupportedVersion { newer }) => {
            json!({"outcome": "unsupported", "newer": newer})
        }
        Err(HeaderRefusal::Rejected(rejection)) => {
            let reason = match rejection {
                Rejection::Framing => "framing",
                Rejection::Json => "json",
                Rejection::NotObject => "not-object",
                Rejection::RetiredPolicyFields => "retired-policy-fields",
                Rejection::NotSessionHeader => "not-session-header",
            };
            json!({"outcome": "rejected", "reason": reason})
        }
        Err(HeaderRefusal::NativeSubset(limit)) => {
            let limit = match limit {
                SubsetLimit::InvalidUtf8 => "invalid-utf8",
                SubsetLimit::JsonParser => "json-parser",
                SubsetLimit::FloatLexeme => "float-lexeme",
                SubsetLimit::VersionDiagnostic => "version-diagnostic",
            };
            json!({"outcome": "native-subset", "limit": limit})
        }
    }
}

#[test]
fn shared_header_cases_match_on_both_platforms() {
    let text = std::fs::read_to_string(repo_path("conformance/session/header-cases.json"))
        .expect("read header-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse header-cases.json");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["schema", "version", "oracle", "cases", "absolutePaths"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    for row in table["absolutePaths"]
        .as_array()
        .expect("absolutePaths array")
    {
        let row = object(row, "absolutePaths row");
        assert_eq!(keys(row), BTreeSet::from(["path", "posix", "win32"]));
        assert!(row["path"].is_string() && row["posix"].is_boolean() && row["win32"].is_boolean());
    }

    let cases = table["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty());
    let mut ids = BTreeSet::new();
    let mut subsets = 0;
    for case in cases {
        let case = object(case, "case");
        let id = case
            .get("id")
            .and_then(Value::as_str)
            .expect("case id string");
        assert!(ids.insert(id), "{id}: duplicate case id");
        let allowed = ["id", "ts", "rust", "meta"];
        assert!(
            case.keys()
                .all(|key| allowed.contains(&key.as_str()) || SOURCES.contains(&key.as_str())),
            "{id}: unknown key"
        );
        let rust = case.get("rust");
        if let Some(rust) = rust {
            let rust_object = object(rust, id);
            assert!(
                keys(rust_object) == BTreeSet::from(["outcome", "limit"])
                    && rust["outcome"] == "native-subset"
                    && [
                        "invalid-utf8",
                        "json-parser",
                        "float-lexeme",
                        "version-diagnostic"
                    ]
                    .iter()
                    .any(|limit| rust["limit"] == *limit),
                "{id}: rust may only name a native-subset limit"
            );
            subsets += 1;
        }
        let ts = case.get("ts").unwrap_or_else(|| panic!("{id}: missing ts"));
        let ts_outcomes = [PathPlatform::Posix, PathPlatform::Win32]
            .map(|platform| &ts_outcome(ts, platform, id)["outcome"]);
        assert!(
            rust.is_some() || !ts_outcomes.iter().any(|outcome| *outcome == "type-error"),
            "{id}: a TypeError outcome needs a native-subset override"
        );
        let admits = rust.is_none() && ts_outcomes.iter().any(|outcome| *outcome == "admitted");
        assert_eq!(
            case.contains_key("meta"),
            admits,
            "{id}: meta is required exactly when the case admits"
        );
        if let Some(meta) = case.get("meta") {
            check_meta(meta, id);
        }

        let bytes = record_bytes(case, id);
        for platform in [PathPlatform::Posix, PathPlatform::Win32] {
            let expected = rust.unwrap_or_else(|| ts_outcome(ts, platform, id));
            let result = read_header_record(&bytes, platform);
            assert_eq!(
                &outcome_value(&result),
                expected,
                "{id} ({platform:?}): outcome"
            );
            if let Ok(header) = &result {
                assert_eq!(
                    &header_meta(header),
                    &case["meta"],
                    "{id} ({platform:?}): meta"
                );
            }
        }
    }
    assert!(subsets > 0);
}

#[test]
fn host_platform_is_win32_only_on_windows() {
    assert_eq!(PathPlatform::host() == PathPlatform::Win32, cfg!(windows));
}

#[test]
fn first_record_ends_at_the_first_lf() {
    assert_eq!(first_record(b"a\nb\n"), Some(&b"a\n"[..]));
    assert_eq!(first_record(b"\n"), Some(&b"\n"[..]));
    assert_eq!(first_record(b"no newline"), None);
    let log = std::fs::read(repo_path(FIXTURE)).expect("read fixture log");
    assert_eq!(log.len(), 4533);
    assert_eq!(first_record(&log).map(<[u8]>::len), Some(104));
}
