//! Runs every shared case in `conformance/runtime/request-derivation-cases.json`
//! through `replay_requests`, over the same bytes the TypeScript spec builds.
//! A case's expected outcome is its `rust` override when present, otherwise
//! the requests the TypeScript `replayRequests` helper returns for the same
//! log. Only this test normalizes message IDs; the derivation never reads an
//! expected file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bake_session::{
    HeaderRefusal, PathPlatform, ReplayLimit, ReplayRefusal, ScanLimit, ScanRefusal, SeedRejection,
    replay_requests,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/runtime-conformance/request-derivation-cases";
const ORACLE: &str =
    "normalizeRequests(replayRequests(log)) from packages/core/agent-loop/tests/runtime-fixture.ts";
const FIXTURE_LOG: &str = "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl";
const FIXTURE_EXPECTED: &str =
    "conformance/runtime/request-reconstruction/tool-call-turn/expected-requests.json";
/// The TypeScript spec checks both files' SHA-256; this crate has no hash
/// dependency, so it pins their sizes.
const LOG_BYTES: usize = 4533;
const EXPECTED_BYTES: usize = 2775;
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 200;
const MAX_EDITS: usize = 8;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 9] = [
    "event-type",
    "number",
    "depth",
    "coordinate",
    "repeated-coordinate",
    "config-member",
    "tool-schema",
    "header",
    "codec",
];
/// Causes whose refusal renders TypeScript's exact message, and the number of
/// cases that check it.
const EXACT_CAUSES: [&str; 4] = ["codec", "finish", "uncommitted", "seeded"];
const EXACT_CASES: usize = 15;
const SEED_CHECKS: [(&str, SeedRejection); 32] = [
    ("lossless-json", SeedRejection::LosslessJson),
    ("message-identity", SeedRejection::MessageIdentity),
    ("message-role", SeedRejection::MessageRole),
    ("message-source", SeedRejection::MessageSource),
    ("message-content", SeedRejection::MessageContent),
    ("model-source", SeedRejection::ModelSource),
    ("tool-source", SeedRejection::ToolSource),
    ("tool-result-block", SeedRejection::ToolResultBlock),
    ("tool-call-id", SeedRejection::ToolCallId),
    ("settlement", SeedRejection::Settlement),
    ("header-provider-model", SeedRejection::HeaderProviderModel),
    (
        "header-reasoning-effort",
        SeedRejection::HeaderReasoningEffort,
    ),
    (
        "header-adapter-defaults",
        SeedRejection::HeaderAdapterDefaults,
    ),
    ("header-reason", SeedRejection::HeaderReason),
    ("header-starts-series", SeedRejection::HeaderStartsSeries),
    ("tool-update-data", SeedRejection::ToolUpdateData),
    ("non-surface-marker", SeedRejection::NonSurfaceMarker),
    ("replace-start", SeedRejection::ReplaceStart),
    ("replace-end", SeedRejection::ReplaceEnd),
    ("replace-order", SeedRejection::ReplaceOrder),
    ("replace-sources", SeedRejection::ReplaceSources),
    ("tool-result-span", SeedRejection::ToolResultSpan),
    ("tool-result-target", SeedRejection::ToolResultTarget),
    ("tool-result-rest", SeedRejection::ToolResultRest),
    ("system-head", SeedRejection::SystemHead),
    ("tool-update-header", SeedRejection::ToolUpdateHeader),
    ("tool-update-stale", SeedRejection::ToolUpdateStale),
    ("tool-update-baseline", SeedRejection::ToolUpdateBaseline),
    ("tool-update-change", SeedRejection::ToolUpdateChange),
    ("tool-update-anchor", SeedRejection::ToolUpdateAnchor),
    ("tool-update-required", SeedRejection::ToolUpdateRequired),
    ("projection-required", SeedRejection::ProjectionRequired),
];

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

struct Fixture {
    /// The header record and rows, without newlines.
    lines: Vec<String>,
    requests: Vec<Value>,
}

fn fixture() -> Fixture {
    let log = std::fs::read_to_string(repo_path(FIXTURE_LOG)).expect("read fixture log");
    let expected =
        std::fs::read_to_string(repo_path(FIXTURE_EXPECTED)).expect("read fixture expectation");
    assert_eq!(log.len(), LOG_BYTES, "fixture log size");
    assert_eq!(expected.len(), EXPECTED_BYTES, "fixture expectation size");
    let mut lines: Vec<String> = log.split('\n').map(str::to_owned).collect();
    assert_eq!(lines.pop().as_deref(), Some(""), "fixture log ends with LF");
    assert_eq!(lines.len(), 17, "header and 16 rows");
    let expected: Value = serde_json::from_str(&expected).expect("parse fixture expectation");
    let requests = expected["requests"]
        .as_array()
        .expect("fixture expectation requests")
        .clone();
    Fixture { lines, requests }
}

fn table() -> Map<String, Value> {
    let text = std::fs::read_to_string(repo_path(
        "conformance/runtime/request-derivation-cases.json",
    ))
    .expect("read request-derivation-cases.json");
    let table: Value = serde_json::from_str(&text).expect("parse request-derivation-cases.json");
    let fields = object(&table, "table").clone();
    assert_eq!(
        keys(&fields),
        BTreeSet::from(["schema", "version", "history", "oracle", "fixture", "cases"])
    );
    assert_eq!(fields["schema"], SCHEMA);
    assert_eq!(fields["version"], 2);
    assert!(
        fields["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    assert_eq!(fields["oracle"], ORACLE);
    assert_eq!(
        fields["fixture"],
        serde_json::json!({"log": FIXTURE_LOG, "expected": FIXTURE_EXPECTED})
    );
    fields
}

/// Resolve a JSON pointer without `~` escapes to its parent container and final token.
fn parent<'a>(root: &'a mut Value, pointer: &str) -> (&'a mut Value, String) {
    assert!(
        pointer.starts_with('/') && !pointer.contains('~'),
        "unsupported pointer {pointer}"
    );
    let mut tokens: Vec<&str> = pointer[1..].split('/').collect();
    let key = tokens.pop().expect("pointer token").to_owned();
    let mut node = root;
    for token in tokens {
        node = match node {
            Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
            Value::Object(fields) => fields.get_mut(token),
            _ => None,
        }
        .unwrap_or_else(|| panic!("{pointer} does not exist"));
    }
    (node, key)
}

fn array_index(key: &str, limit: usize, pointer: &str) -> usize {
    let index = key
        .parse::<usize>()
        .ok()
        .filter(|index| *index <= limit && (key == "0" || !key.starts_with('0')));
    index.unwrap_or_else(|| panic!("{pointer} is not an array position"))
}

enum Operation {
    Set(Value),
    Insert(Value),
    Remove,
}

fn apply_edit(root: &mut Value, pointer: &str, operation: Operation) {
    let (node, key) = parent(root, pointer);
    match (node, operation) {
        (Value::Array(items), Operation::Insert(value)) => {
            let index = array_index(&key, items.len(), pointer);
            items.insert(index, value);
        }
        (Value::Array(items), Operation::Remove) if !items.is_empty() => {
            let index = array_index(&key, items.len() - 1, pointer);
            items.remove(index);
        }
        (Value::Array(items), Operation::Set(value)) if !items.is_empty() => {
            let index = array_index(&key, items.len() - 1, pointer);
            items[index] = value;
        }
        (Value::Object(fields), Operation::Set(value)) => {
            fields.insert(key, value);
        }
        (Value::Object(fields), Operation::Remove) => {
            assert!(
                fields.shift_remove(&key).is_some(),
                "{pointer} does not exist"
            );
        }
        _ => panic!("{pointer}: edit does not apply to its parent"),
    }
}

/// Row edits re-serialize the row with `JSON.stringify` in TypeScript, so
/// their values, and here their whole rows, may hold only safe integers.
fn assert_integer_numbers(value: &Value, context: &str) {
    let mut pending = vec![value];
    while let Some(item) = pending.pop() {
        match item {
            Value::Number(number) => assert!(
                number.as_u64().is_some_and(|n| n < 1 << 53)
                    || number.as_i64().is_some_and(|n| n.unsigned_abs() < 1 << 53),
                "{context}: {number} is not a safe integer"
            ),
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => pending.extend(fields.values()),
            _ => {}
        }
    }
}

/// `JSON.stringify` text of a row whose numbers are safe integers and whose
/// objects have no array-index keys, which JavaScript would enumerate first.
fn stringify(row: &Value, id: &str) -> String {
    assert_integer_numbers(row, id);
    let mut pending = vec![row];
    while let Some(item) = pending.pop() {
        match item {
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => {
                assert!(
                    fields.keys().all(|key| key.parse::<u32>().is_err()),
                    "{id}: a re-serialized row has an array-index key"
                );
                pending.extend(fields.values());
            }
            _ => {}
        }
    }
    serde_json::to_string(row).expect("serialize row")
}

/// The case's log bytes, as the TypeScript spec builds them: the lines joined
/// by LF with a final LF, then any unterminated `tail`. A row edited by value
/// is parsed, edited, and re-serialized; every other row keeps its exact text.
fn case_log(case: &Map<String, Value>, fixture: &Fixture, id: &str) -> Vec<u8> {
    let mut lines: Vec<String>;
    let mut values: Vec<Option<Value>>;
    let mut tail = String::new();
    if case["log"] == "fixture" {
        lines = fixture.lines.clone();
        values = vec![None; lines.len()];
        let edits = case["edits"].as_array().expect("fixture case edits");
        assert!(edits.len() <= MAX_EDITS, "{id}: too many edits");
        let rows = fixture.lines.len() - 1;
        for edit in edits {
            let fields = object(edit, id);
            let row = || {
                let row = edit["row"].as_u64().expect("edit row") as usize;
                assert!(row < rows, "{id}: row {row} is outside the fixture");
                row + 1
            };
            match keys(fields).into_iter().collect::<Vec<_>>().as_slice() {
                ["header"] => lines[0] = edit["header"].as_str().expect("header").to_owned(),
                ["tail"] => edit["tail"].as_str().expect("tail").clone_into(&mut tail),
                ["truncate"] => {
                    let keep = edit["truncate"].as_u64().expect("truncate") as usize;
                    assert!(keep < rows, "{id}: truncate keeps fewer rows");
                    lines.truncate(keep + 1);
                    values.truncate(keep + 1);
                }
                ["row", "text"] => {
                    let line = row();
                    lines[line] = edit["text"].as_str().expect("text").to_owned();
                    values[line] = None;
                }
                ["pointer", "row", "value"] => {
                    assert_integer_numbers(&edit["value"], id);
                    let line = row();
                    let value = values[line]
                        .get_or_insert_with(|| serde_json::from_str(&lines[line]).expect("row"));
                    let pointer = edit["pointer"].as_str().expect("pointer");
                    apply_edit(value, pointer, Operation::Set(edit["value"].clone()));
                }
                ["pointer", "remove", "row"] if edit["remove"] == true => {
                    let line = row();
                    let value = values[line]
                        .get_or_insert_with(|| serde_json::from_str(&lines[line]).expect("row"));
                    apply_edit(
                        value,
                        edit["pointer"].as_str().expect("pointer"),
                        Operation::Remove,
                    );
                }
                _ => panic!("{id}: invalid edit {edit}"),
            }
        }
    } else {
        assert!(!case.contains_key("edits"), "{id}: own log takes no edits");
        lines = case["log"]
            .as_array()
            .expect("log lines")
            .iter()
            .map(|line| line.as_str().expect("log line").to_owned())
            .collect();
        assert!(!lines.is_empty(), "{id}: log has a header");
        values = vec![None; lines.len()];
    }
    let mut log = String::new();
    for (line, value) in lines.iter().zip(values) {
        let line = value.map_or_else(|| line.clone(), |row| stringify(&row, id));
        assert!(!line.contains('\n'), "{id}: a line holds a newline");
        log.push_str(&line);
        log.push('\n');
    }
    assert!(!tail.contains('\n'), "{id}: the tail holds a newline");
    log.push_str(&tail);
    log.into_bytes()
}

fn expected_requests(ts: &Value, fixture: &Fixture, id: &str) -> Vec<Value> {
    let fields = object(ts, id);
    if keys(fields) == BTreeSet::from(["outcome", "requests"]) {
        return ts["requests"].as_array().expect("requests").clone();
    }
    assert_eq!(
        keys(fields),
        BTreeSet::from(["outcome", "requests", "edits"]),
        "{id}"
    );
    assert_eq!(ts["requests"], "fixture", "{id}");
    let mut requests = Value::Array(fixture.requests.clone());
    for edit in ts["edits"].as_array().expect("request edits") {
        let fields = object(edit, id);
        let pointer = edit["pointer"].as_str().expect("pointer");
        let operation = match keys(fields).into_iter().collect::<Vec<_>>().as_slice() {
            ["pointer", "value"] => Operation::Set(edit["value"].clone()),
            ["insert", "pointer"] => Operation::Insert(edit["insert"].clone()),
            ["pointer", "remove"] if edit["remove"] == true => Operation::Remove,
            _ => panic!("{id}: invalid request edit {edit}"),
        };
        apply_edit(&mut requests, pointer, operation);
    }
    match requests {
        Value::Array(requests) => requests,
        _ => unreachable!("requests stay an array"),
    }
}

/// `randomUUID()` output: lowercase hex, version 4, RFC 4122 variant.
fn is_generated_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `normalizeRequests`: one bijection from generated message IDs to
/// `message-N`, numbered by first use across the whole request sequence.
fn normalize(requests: &[Value], id: &str) -> Vec<Value> {
    let mut placeholders: BTreeMap<String, String> = BTreeMap::new();
    let mut requests = requests.to_vec();
    for request in &mut requests {
        for message in request["messages"]
            .as_array_mut()
            .expect("request messages")
        {
            let raw = message["id"].as_str().expect("message id").to_owned();
            assert!(is_generated_id(&raw), "{id}: {raw} is not a generated id");
            let next = format!("message-{}", placeholders.len() + 1);
            message["id"] = Value::String(placeholders.entry(raw).or_insert(next).clone());
        }
    }
    requests
}

/// The case-table name of a refusal: `limit:<name>` or `cause:<name>`.
fn classify(refusal: &ReplayRefusal) -> String {
    match refusal {
        ReplayRefusal::Scan(ScanRefusal::Header(HeaderRefusal::NativeSubset(_))) => {
            "limit:header".to_owned()
        }
        ReplayRefusal::Scan(ScanRefusal::Header(_)) => "cause:header".to_owned(),
        ReplayRefusal::Scan(ScanRefusal::NativeSubset {
            limit: ScanLimit::Codec(_),
            ..
        }) => "limit:codec".to_owned(),
        ReplayRefusal::Scan(ScanRefusal::NativeSubset { limit, .. }) => {
            format!("limit:scan/{limit:?}")
        }
        ReplayRefusal::Scan(ScanRefusal::Finish(_)) => "cause:finish".to_owned(),
        ReplayRefusal::Scan(_) => "cause:codec".to_owned(),
        ReplayRefusal::Uncommitted { .. } => "cause:uncommitted".to_owned(),
        ReplayRefusal::Seeded => "cause:seeded".to_owned(),
        ReplayRefusal::Seed { rejection, .. } => SEED_CHECKS
            .iter()
            .find(|(_, check)| check == rejection)
            .map_or_else(
                || format!("cause:seed/unclassified:{rejection:?}"),
                |(name, _)| format!("cause:seed/{name}"),
            ),
        ReplayRefusal::NoLaterSettlement { .. } => "cause:no-later-settlement".to_owned(),
        ReplayRefusal::NoRequestHeader { .. } => "cause:no-request-header".to_owned(),
        ReplayRefusal::NativeSubset { limit, .. } => format!(
            "limit:{}",
            match limit {
                ReplayLimit::EventType => "event-type",
                ReplayLimit::Number => "number",
                ReplayLimit::Depth => "depth",
                ReplayLimit::Coordinate => "coordinate",
                ReplayLimit::RepeatedCoordinate => "repeated-coordinate",
                ReplayLimit::ConfigMember => "config-member",
                ReplayLimit::ToolSchema => "tool-schema",
            }
        ),
    }
}

fn replay(log: &[u8]) -> Result<Vec<Value>, ReplayRefusal> {
    replay_requests(log, PathPlatform::host(), SOURCE_BUDGET).map(|requests| {
        requests
            .iter()
            .map(bake_session::Request::to_json)
            .collect()
    })
}

#[test]
fn shared_cases_derive_like_replay_requests() {
    let table = table();
    let fixture = fixture();
    let cases = table["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), CASE_COUNT, "case count");
    let mut ids = BTreeSet::new();
    let mut limits = BTreeSet::new();
    let mut causes = BTreeSet::new();
    let mut limited_outcomes = BTreeSet::new();
    let mut exact = 0;
    for case in cases {
        let fields = object(case, "case");
        let id = case["id"].as_str().expect("case id");
        assert!(ids.insert(id), "{id}: duplicate id");
        let allowed = BTreeSet::from(["id", "log", "edits", "ts", "rust"]);
        assert!(keys(fields).is_subset(&allowed), "{id}: unknown keys");
        let actual = replay(&case_log(fields, &fixture, id));
        let ts_outcome = case["ts"]["outcome"].as_str().expect("ts outcome");
        match case.get("rust") {
            None => {
                assert_eq!(
                    ts_outcome, "requests",
                    "{id}: a rejection needs an override"
                );
                let requests = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                assert_eq!(
                    normalize(&requests, id),
                    expected_requests(&case["ts"], &fixture, id),
                    "{id}"
                );
            }
            Some(rust) => {
                let refusal = actual.expect_err(id);
                let expected = match rust["outcome"].as_str() {
                    Some("native-subset") => {
                        assert_eq!(keys(object(rust, id)), BTreeSet::from(["outcome", "limit"]));
                        let limit = rust["limit"].as_str().expect("limit");
                        assert!(LIMITS.contains(&limit), "{id}: unknown limit {limit}");
                        limits.insert(limit.to_owned());
                        limited_outcomes.insert(ts_outcome.to_owned());
                        format!("limit:{limit}")
                    }
                    Some("rejected") => {
                        assert_eq!(ts_outcome, "rejected", "{id}: cause without a rejection");
                        assert!(case["ts"]["message"].is_string(), "{id}: rejection message");
                        let cause = rust["cause"].as_str().expect("cause");
                        causes.insert(cause.to_owned());
                        // Scan-level causes claim TypeScript's exact message.
                        if EXACT_CAUSES.contains(&cause) {
                            assert_eq!(
                                refusal.message().as_deref(),
                                case["ts"]["message"].as_str(),
                                "{id}: exact message"
                            );
                            exact += 1;
                        }
                        format!("cause:{cause}")
                    }
                    _ => panic!("{id}: invalid override {rust}"),
                };
                assert_eq!(classify(&refusal), expected, "{id}: {refusal:?}");
            }
        }
    }
    assert_eq!(exact, EXACT_CASES, "exact scan-level messages");
    let all_causes: BTreeSet<String> = ["header", "no-later-settlement", "no-request-header"]
        .into_iter()
        .chain(EXACT_CAUSES)
        .map(str::to_owned)
        .chain(SEED_CHECKS.iter().map(|(name, _)| format!("seed/{name}")))
        .collect();
    assert_eq!(causes, all_causes, "every cause is witnessed");
    assert_eq!(
        limits,
        LIMITS.iter().map(|limit| (*limit).to_owned()).collect(),
        "every limit is witnessed"
    );
    assert_eq!(
        limited_outcomes,
        BTreeSet::from(["requests".to_owned(), "rejected".to_owned()]),
        "limits cover accepted and rejected input"
    );
}

#[test]
fn fixture_rows_reserialize_to_their_text() {
    for line in fixture().lines {
        let row: Value = serde_json::from_str(&line).expect("fixture line");
        assert_eq!(stringify(&row, "fixture"), line);
    }
}

#[test]
fn requests_keep_the_logged_message_ids() {
    let fixture = fixture();
    let log = format!("{}\n", fixture.lines.join("\n"));
    let rows: Vec<Value> = fixture.lines[1..]
        .iter()
        .map(|line| serde_json::from_str(line).expect("row"))
        .collect();
    let logged = |seq: usize| {
        let data = &rows[seq]["data"];
        data.get("message").unwrap_or(data)["id"].clone()
    };
    let requests = replay(log.as_bytes()).expect("fixture requests");
    let ids: Vec<Vec<Value>> = requests
        .iter()
        .map(|request| {
            request["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .map(|message| message["id"].clone())
                .collect()
        })
        .collect();
    assert_eq!(
        ids,
        [
            vec![logged(4), logged(5)],
            vec![logged(4), logged(5), logged(8), logged(10)]
        ]
    );
}
