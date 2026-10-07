//! Replays the committed real-runtime captures in
//! `conformance/runtime/request-reconstruction/` through `replay_requests` and
//! compares each request with the scenario's hand-written
//! `expected-requests.json`. The test reads only those committed files and
//! owns its normalizer: generated message IDs, and tool-history anchors that
//! name a message of the same request, map to `message-N` through one
//! bijection per request sequence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bake_session::{PathPlatform, ReplayRefusal, SeedRejection, replay_requests, scan_log};
use serde_json::{Value, json};

const ROOT: &str = "conformance/runtime/request-reconstruction";
const SCHEMA: &str = "bake/runtime-conformance/requests";
const SOURCE_BUDGET: usize = 64;

/// A committed capture, pinned here by its sizes and row count.
/// The TypeScript spec also pins the dynamic-tools log by SHA-256.
struct Capture {
    scenario: &'static str,
    log_bytes: usize,
    expected_bytes: usize,
    rows: usize,
}

const TOOL_CALL_TURN: Capture = Capture {
    scenario: "tool-call-turn",
    log_bytes: 4533,
    expected_bytes: 2775,
    rows: 16,
};
const DYNAMIC_TOOLS: Capture = Capture {
    scenario: "dynamic-tools",
    log_bytes: 9755,
    expected_bytes: 9568,
    rows: 38,
};

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

/// The committed log bytes and expected requests.
fn read(capture: &Capture) -> (Vec<u8>, Vec<Value>) {
    let directory = format!("{ROOT}/{}", capture.scenario);
    let log = std::fs::read(repo_path(&format!("{directory}/session.jsonl"))).expect("read log");
    let expected = std::fs::read(repo_path(&format!("{directory}/expected-requests.json")))
        .expect("read expectation");
    assert_eq!(
        log.len(),
        capture.log_bytes,
        "{}: log size",
        capture.scenario
    );
    assert_eq!(
        expected.len(),
        capture.expected_bytes,
        "{}: expectation size",
        capture.scenario
    );
    let rows = log.iter().filter(|byte| **byte == b'\n').count() - 1;
    assert_eq!(rows, capture.rows, "{}: rows", capture.scenario);
    let document: Value = serde_json::from_slice(&expected).expect("parse expectation");
    let fields = document.as_object().expect("expectation object");
    assert_eq!(
        fields.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["schema", "version", "requests"])
    );
    assert_eq!(fields["schema"], SCHEMA);
    assert_eq!(fields["version"], 1);
    (
        log,
        fields["requests"].as_array().expect("requests").clone(),
    )
}

fn replay(log: &[u8]) -> Result<Vec<Value>, ReplayRefusal> {
    let requests = replay_requests(log, PathPlatform::Posix, SOURCE_BUDGET)?;
    Ok(requests.iter().map(|request| request.to_json()).collect())
}

/// Whether `text` has the `randomUUID()` v4 form: lowercase hex, version 4,
/// variant `8`–`b`.
fn is_generated_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// Map each request's message IDs, then its tool-history anchors, through one
/// bijection numbered by first use. An anchor must be a generated ID that a
/// message of the same request carries. Nothing else changes.
fn normalize(requests: &[Value]) -> Result<Vec<Value>, String> {
    let mut placeholders: BTreeMap<String, String> = BTreeMap::new();
    let mut requests = requests.to_vec();
    for (index, request) in requests.iter_mut().enumerate() {
        let request = request.as_object_mut().ok_or("request is not an object")?;
        if request.contains_key("toolUpdates") {
            return Err(format!("/{index}/toolUpdates is not normalized"));
        }
        let mut carried = BTreeSet::new();
        let messages = request
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or(format!("/{index}/messages is not an array"))?;
        for (position, message) in messages.iter_mut().enumerate() {
            let pointer = format!("/{index}/messages/{position}/id");
            let raw = message
                .get("id")
                .and_then(Value::as_str)
                .filter(|raw| is_generated_id(raw))
                .ok_or(format!("{pointer}: not a generated id"))?
                .to_owned();
            let next = format!("message-{}", placeholders.len() + 1);
            message["id"] = Value::String(placeholders.entry(raw.clone()).or_insert(next).clone());
            carried.insert(raw);
        }
        let updates = request
            .get_mut("toolHistory")
            .and_then(|history| history.get_mut("updates"))
            .and_then(Value::as_array_mut)
            .ok_or(format!("/{index}/toolHistory/updates is not an array"))?;
        for (position, update) in updates.iter_mut().enumerate() {
            let pointer = format!("/{index}/toolHistory/updates/{position}/afterMessageId");
            let raw = update
                .get("afterMessageId")
                .and_then(Value::as_str)
                .filter(|raw| is_generated_id(raw))
                .ok_or(format!("{pointer}: not a generated id"))?;
            if !carried.contains(raw) {
                return Err(format!("{pointer}: names no message of this request"));
            }
            update["afterMessageId"] = Value::String(placeholders[raw].clone());
        }
    }
    Ok(requests)
}

/// The JSON pointer of the first difference, or `None` when equal. Object
/// member order is immaterial.
fn first_difference(expected: &Value, actual: &Value, pointer: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Array(left), Value::Array(right)) if left.len() == right.len() => left
            .iter()
            .zip(right)
            .enumerate()
            .find_map(|(index, (left, right))| {
                first_difference(left, right, &format!("{pointer}/{index}"))
            }),
        (Value::Object(left), Value::Object(right))
            if left.keys().collect::<BTreeSet<_>>() == right.keys().collect::<BTreeSet<_>>() =>
        {
            left.iter().find_map(|(key, value)| {
                first_difference(value, &right[key], &format!("{pointer}/{key}"))
            })
        }
        _ => (expected != actual).then(|| pointer.to_owned()),
    }
}

/// The log's row types and data, in order.
fn rows(log: &[u8]) -> Vec<(String, Value)> {
    let scan = scan_log(log, PathPlatform::Posix, SOURCE_BUDGET).expect("scan");
    scan.rows()
        .iter()
        .map(|row| {
            (
                row["type"].as_str().expect("type").to_owned(),
                row["data"].clone(),
            )
        })
        .collect()
}

/// Replace `from` with the same-length `to` in the one row with seq `seq`.
fn replace_in_row(log: &[u8], seq: usize, from: &str, to: &str) -> Vec<u8> {
    let text = std::str::from_utf8(log).expect("UTF-8 log");
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let row = &mut lines[seq + 1];
    assert!(row.contains(&format!("\"seq\":{seq},")), "row {seq}");
    assert_eq!(row.matches(from).count(), 1, "row {seq}: {from}");
    assert_eq!(from.len(), to.len(), "same length");
    *row = row.replace(from, to);
    lines.join("\n").into_bytes()
}

#[test]
fn committed_captures_replay_to_their_expectations() {
    for capture in [TOOL_CALL_TURN, DYNAMIC_TOOLS] {
        let (log, expected) = read(&capture);
        let replayed = replay(&log).unwrap_or_else(|refusal| panic!("{refusal:?}"));
        let normalized = normalize(&replayed).expect("normalize");
        assert_eq!(
            first_difference(&Value::Array(expected), &Value::Array(normalized), ""),
            None,
            "{}",
            capture.scenario
        );
    }
}

#[test]
fn dynamic_tools_log_holds_the_scenario_rows() {
    let (log, _) = read(&DYNAMIC_TOOLS);
    let rows = rows(&log);
    let seqs = |kind: &str| -> Vec<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, (row, _))| row == kind)
            .map(|(seq, _)| seq)
            .collect()
    };
    assert_eq!(seqs("request/header"), [6, 13, 23, 33]);
    assert_eq!(seqs("request/tool-update"), [14, 24, 34]);
    assert_eq!(
        seqs("assistant/message"),
        [8, 15, 25, 35],
        "settlement cuts"
    );
    assert_eq!(seqs("system/message"), [4]);
    let reasons: Vec<&Value> = seqs("request/header")
        .into_iter()
        .map(|seq| &rows[seq].1)
        .inspect(|data| assert!(data.get("startsSeries").is_none()))
        .map(|data| &data["reason"])
        .collect();
    assert_eq!(
        reasons,
        [
            &json!("initial"),
            &json!("change"),
            &json!("change"),
            &json!("change")
        ]
    );
    let updates: Vec<Value> = seqs("request/tool-update")
        .into_iter()
        .map(|seq| {
            let data = &rows[seq].1;
            json!([data["headerSeq"], data["additions"], data["removals"]])
        })
        .collect();
    assert_eq!(
        updates,
        [
            json!([13, ["fetch"], []]),
            json!([23, [], ["fetch"]]),
            json!([33, ["fetch"], []])
        ]
    );
}

#[test]
fn a_redeclared_fetch_in_the_last_header_resets_the_history() {
    let (log, expected) = read(&DYNAMIC_TOOLS);
    let mutated = replace_in_row(
        &log,
        33,
        "\"description\":\"fetch a url\"",
        "\"description\":\"fetch a urx\"",
    );
    let replayed = normalize(&replay(&mutated).expect("replay")).expect("normalize");
    let mut changed = expected[3]["tools"][0].clone();
    changed["description"] = json!("fetch a urx");
    assert_eq!(
        replayed[3]["toolHistory"],
        json!({"tools": [changed, expected[0]["tools"][0]], "updates": []})
    );
    assert_eq!(replayed[..3], expected[..3]);
    assert_eq!(
        first_difference(&Value::Array(expected), &Value::Array(replayed), "").as_deref(),
        Some("/3/tools/0/description")
    );
}

#[test]
fn an_update_anchored_to_an_earlier_message_is_refused() {
    let (log, _) = read(&DYNAMIC_TOOLS);
    let rows = rows(&log);
    let anchor = |seq: usize| {
        rows[seq].1["afterMessageId"]
            .as_str()
            .expect("anchor")
            .to_owned()
    };
    let mutated = replace_in_row(&log, 24, &anchor(24), &anchor(14));
    assert_eq!(
        replay(&mutated),
        Err(ReplayRefusal::Seed {
            seq: 24,
            rejection: SeedRejection::ToolUpdateAnchor
        })
    );
}

#[test]
fn a_dropped_request_or_missing_settlement_fails() {
    let (log, expected) = read(&DYNAMIC_TOOLS);
    let replayed = normalize(&replay(&log).expect("replay")).expect("normalize");
    assert_eq!(
        first_difference(&json!(expected[..3]), &Value::Array(replayed), ""),
        Some(String::new()),
        "a three-request expectation differs at the root"
    );
    // Cutting the log before the last settlement leaves turn 3's step without one.
    let end = log
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(35)
        .map(|(index, _)| index + 1)
        .expect("row 34 ends");
    assert_eq!(
        replay(&log[..end]),
        Err(ReplayRefusal::NoLaterSettlement { turn: 3, step: 1 })
    );
}

#[test]
fn the_normalizer_refuses_foreign_anchors() {
    let (log, expected) = read(&DYNAMIC_TOOLS);
    let replayed = replay(&log).expect("replay");
    let anchor = "/1/toolHistory/updates/0/afterMessageId";
    let with_anchor = |value: Value| {
        let mut requests = Value::Array(replayed.clone());
        *requests.pointer_mut(anchor).expect("anchor") = value;
        requests.as_array().expect("requests").clone()
    };
    let later = replayed[3]["messages"][7]["id"].clone();
    let fresh = json!("0f8e2c1a-4b3d-4e5f-8a9b-1c2d3e4f5a6b");
    for value in [later, fresh] {
        assert_eq!(
            normalize(&with_anchor(value)),
            Err(format!("{anchor}: names no message of this request"))
        );
    }
    for value in [json!("message-4"), Value::Null] {
        assert_eq!(
            normalize(&with_anchor(value)),
            Err(format!("{anchor}: not a generated id"))
        );
    }
    let mut with_updates = replayed.clone();
    with_updates[0]["toolUpdates"] = json!([]);
    assert_eq!(
        normalize(&with_updates),
        Err("/0/toolUpdates is not normalized".to_owned())
    );
    // An anchor left raw, or an expected anchor naming another message, differs there.
    let mut raw = normalize(&replayed).expect("normalize");
    *raw[1]
        .pointer_mut("/toolHistory/updates/0/afterMessageId")
        .expect("anchor") = replayed[1]
        .pointer("/toolHistory/updates/0/afterMessageId")
        .cloned()
        .expect("raw");
    let mut moved = Value::Array(expected.clone());
    *moved.pointer_mut(anchor).expect("anchor") = json!("message-2");
    for (left, right) in [
        (Value::Array(expected.clone()), Value::Array(raw)),
        (
            moved,
            Value::Array(normalize(&replayed).expect("normalize")),
        ),
    ] {
        assert_eq!(first_difference(&left, &right, "").as_deref(), Some(anchor));
    }
}
