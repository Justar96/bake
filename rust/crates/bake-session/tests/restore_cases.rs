//! Runs every shared case in `conformance/session/restore-cases.json` through
//! `restore_plain_log`, over the same bytes the TypeScript spec builds. A
//! case's expected outcome is its `rust` override when present, otherwise the
//! hand-written restored state the TypeScript `restorePlainLog` helper also
//! meets, with messages also compared as serialized text, since `Value`
//! equality ignores member order. An `image/offload` rejection also claims
//! TypeScript's exact message. Nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    HeaderOrigin, OffloadRejection, PathPlatform, ReplayRefusal, RestoreLimit, RestoreRefusal,
    RestoredLog, SeedRejection, SessionHeader, Unsupported, replay_requests, restore_plain_log,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/restore-cases";
const ORACLE: &str = "restorePlainLog(log) in packages/session/session-persistence-jsonl/tests/restore-conformance.spec.ts: scanLog, validateStoredEvents, interruptedTurnClosers, then Session.fromRestore(..., \"detached\", currentSessionMessageProjections)";
/// Each capture and its size; the TypeScript spec checks their SHA-256.
const LOGS: [(&str, &str, usize); 3] = [
    (
        "tool-call-turn",
        "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl",
        4533,
    ),
    (
        "dynamic-tools",
        "conformance/runtime/request-reconstruction/dynamic-tools/session.jsonl",
        9755,
    ),
    (
        "retry-attempt",
        "conformance/runtime/request-reconstruction/retry-attempt/session.jsonl",
        3102,
    ),
];
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 124;
const SOURCE_BUDGET: usize = 64;
/// Cases whose `image/offload` rejection message is compared exactly.
const OFFLOAD_MESSAGES: usize = 28;
/// Refusals whose TypeScript message names no seq, and the row each names.
const UNNAMED_SEQS: [(&str, u64); 4] = [
    ("marker-on-opaque-type-after-last-settlement", 38),
    ("image-offload-surface-marker", 16),
    ("known-ignorable-tool-update-marker", 14),
    ("known-ignorable-routing-decision-marker", 16),
];
const LIMITS: [(&str, RestoreLimit); 6] = [
    ("number", RestoreLimit::Number),
    ("coordinate", RestoreLimit::Coordinate),
    ("tool-schema", RestoreLimit::ToolSchema),
    ("context", RestoreLimit::Context),
    ("repair", RestoreLimit::Repair),
    ("projection", RestoreLimit::Projection),
];
const SEED_CHECKS: [(&str, SeedRejection); 30] = [
    ("message-identity", SeedRejection::MessageIdentity),
    ("message-role", SeedRejection::MessageRole),
    ("message-source", SeedRejection::MessageSource),
    ("message-content", SeedRejection::MessageContent),
    ("model-source", SeedRejection::ModelSource),
    ("tool-source", SeedRejection::ToolSource),
    ("tool-result-block", SeedRejection::ToolResultBlock),
    ("tool-call-id", SeedRejection::ToolCallId),
    ("tool-update-data", SeedRejection::ToolUpdateData),
    ("non-surface-marker", SeedRejection::NonSurfaceMarker),
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
];
/// The first ten checks are the ones adoption runs.
const STORED_CHECKS: usize = 10;

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

fn index(value: &Value, context: &str) -> usize {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| panic!("{context}: expected a count"))
}

struct Case {
    id: String,
    log: Vec<u8>,
    /// The edited rows as parsed, for `$log` references; `Null` when a row
    /// does not parse.
    rows: Vec<Value>,
    entry: Map<String, Value>,
}

/// Apply a case's text edits to its capture, as the TypeScript spec does.
fn build(entry: &Map<String, Value>, id: &str) -> (Vec<u8>, Vec<String>) {
    let name = text(&entry["log"], id);
    let (_, path, size) = LOGS
        .iter()
        .find(|(log, _, _)| *log == name)
        .unwrap_or_else(|| panic!("{id}: unknown log {name}"));
    let source = std::fs::read_to_string(repo_path(path)).expect("read capture");
    assert_eq!(source.len(), *size, "{path} changed");
    let mut lines: Vec<String> = source
        .strip_suffix('\n')
        .expect("final LF")
        .split('\n')
        .map(str::to_owned)
        .collect();
    let mut header = lines.remove(0);
    let mut rows = lines;
    let mut tail = String::new();
    let edits = entry["edits"].as_array().expect("edits");
    for edit in edits {
        let edit = object(edit, id);
        let line = |key: &str| {
            let value = text(&edit[key], id);
            assert!(!value.contains('\n'), "{id}: {key} holds an LF");
            value.to_owned()
        };
        match keys(edit).into_iter().collect::<Vec<_>>().as_slice() {
            ["truncate"] => {
                let count = index(&edit["truncate"], id);
                assert!(count <= rows.len(), "{id}: truncate past the end");
                rows.truncate(count);
            }
            ["header"] => header = line("header"),
            ["append"] => rows.push(line("append")),
            ["tail"] => tail = line("tail"),
            ["row", "text"] => {
                let row = index(&edit["row"], id);
                assert!(row < rows.len(), "{id}: no row {row}");
                rows[row] = line("text");
            }
            ["find", "replace", "row"] => {
                let row = index(&edit["row"], id);
                let find = line("find");
                assert!(!find.is_empty(), "{id}: empty find");
                assert_eq!(rows[row].matches(&find).count(), 1, "{id}: find once");
                rows[row] = rows[row].replacen(&find, &line("replace"), 1);
            }
            other => panic!("{id}: invalid edit {other:?}"),
        }
    }
    let mut log = String::new();
    for line in std::iter::once(&header).chain(&rows) {
        log.push_str(line);
        log.push('\n');
    }
    log.push_str(&tail);
    (log.into_bytes(), rows)
}

fn load() -> Vec<Case> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/restore-cases.json")).expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "logs", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 5);
    assert!(
        table["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    assert_eq!(table["oracle"], ORACLE);
    let logs = object(&table["logs"], "logs");
    assert_eq!(logs.len(), LOGS.len());
    for (name, path, _) in LOGS {
        assert_eq!(logs[name], path);
    }
    let cases = table["cases"].as_array().expect("cases");
    cases
        .iter()
        .map(|entry| {
            let entry = object(entry, "case").clone();
            let id = text(&entry["id"], "case id").to_owned();
            let allowed =
                BTreeSet::from(["id", "log", "edits", "ts", "rust", "production", "note"]);
            assert!(keys(&entry).is_subset(&allowed), "{id}: unknown keys");
            let (log, rows) = build(&entry, &id);
            let rows = rows
                .iter()
                .map(|row| serde_json::from_str(row).unwrap_or(Value::Null))
                .collect();
            Case {
                id,
                log,
                rows,
                entry,
            }
        })
        .collect()
}

/// Resolve a JSON pointer without `~` escapes, refusing a missing member.
fn at<'a>(root: &'a Value, pointer: &str, id: &str) -> &'a Value {
    assert!(
        pointer.starts_with('/') && !pointer.contains('~'),
        "{id}: unsupported pointer {pointer}"
    );
    pointer.split('/').skip(1).fold(root, |node, key| {
        let next = match node {
            Value::Array(items) => key.parse::<usize>().ok().and_then(|index| items.get(index)),
            Value::Object(fields) => fields.get(key),
            _ => None,
        };
        next.unwrap_or_else(|| panic!("{id}: {pointer} does not exist"))
    })
}

/// Replace each `{ "$log": pointer }` and `{ "$closer": pointer }` with the
/// value it names.
fn resolve(value: &Value, case: &Case, closers: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve(item, case, closers))
                .collect(),
        ),
        Value::Object(fields) => {
            if fields.keys().any(|key| key.starts_with('$')) {
                assert_eq!(fields.len(), 1, "{}: invalid reference", case.id);
                let (key, pointer) = fields.iter().next().expect("one member");
                let pointer = text(pointer, &case.id);
                return match key.as_str() {
                    "$log" => at(&Value::Array(case.rows.clone()), pointer, &case.id).clone(),
                    "$closer" => at(closers, pointer, &case.id).clone(),
                    _ => panic!("{}: invalid reference {key}", case.id),
                };
            }
            Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| (key.clone(), resolve(item, case, closers)))
                    .collect(),
            )
        }
        _ => value.clone(),
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

/// The restored state in the table's form.
fn restored_value(restored: &RestoredLog) -> Value {
    let stored = restored.stored();
    json!({
        "outcome": "restored",
        "header": header_meta(stored.header()),
        "inheritedEventCount": stored.inherited_event_count(),
        "committedBytes": stored.committed_bytes(),
        "storedEventCount": stored.rows().len(),
        "closers": restored.closers(),
        "endSeedAppended": restored.end_seed_appended(),
        "messages": restored.messages(),
        "requestHeader": restored.request_header(),
        "toolHistory": restored.tool_history(),
        "requestContext": restored.request_context(),
    })
}

/// The seq a TypeScript refusal message names, when it names one.
fn message_seq(message: &str) -> Option<u64> {
    ["(seq ", "at seq ", "at index "].iter().find_map(|marker| {
        let start = message.find(marker)? + marker.len();
        let digits: String = message[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

/// The table's name for an `image/offload` rejection.
const fn offload_check(rejection: OffloadRejection) -> &'static str {
    match rejection {
        OffloadRejection::Data => "image-offload-data",
        OffloadRejection::Target => "image-offload-target",
        OffloadRejection::DuplicateTarget { .. } => "image-offload-duplicate",
        OffloadRejection::NotCurrent { .. } => "image-offload-not-current",
        OffloadRejection::TargetType { .. } => "image-offload-target-type",
        OffloadRejection::ImageIndexes => "image-offload-indexes",
        OffloadRejection::AlreadyOffloaded { .. } => "image-offload-already-offloaded",
        OffloadRejection::MissingIndex { .. } => "image-offload-missing-index",
    }
}

/// Check a claimed rejection; returns whether its exact message was compared.
fn check_rejection(case: &Case, cause: &str, refusal: &RestoreRefusal) -> bool {
    let id = &case.id;
    let message = case.entry["ts"]["message"].as_str();
    let seq = match (cause, refusal) {
        ("scan", RestoreRefusal::Scan(scan)) => {
            assert_eq!(scan.message().as_deref(), message, "{id}");
            return false;
        }
        ("unsupported/unknown-type", RestoreRefusal::Unsupported { seq, cause })
            if *cause == Unsupported::UnknownType =>
        {
            *seq
        }
        ("unsupported/fallback-header", RestoreRefusal::Unsupported { seq, cause })
            if *cause == Unsupported::FallbackHeader =>
        {
            *seq
        }
        (_, RestoreRefusal::Stored { seq, rejection }) => {
            let check = cause
                .strip_prefix("stored/")
                .unwrap_or_else(|| panic!("{id}: {refusal:?}"));
            let (_, expected) = SEED_CHECKS[..STORED_CHECKS]
                .iter()
                .find(|(name, _)| *name == check)
                .unwrap_or_else(|| panic!("{id}: unknown stored check {check}"));
            assert_eq!(rejection, expected, "{id}");
            *seq
        }
        (
            _,
            RestoreRefusal::Restore {
                seq,
                rejection: SeedRejection::ImageOffload(rejection),
            },
        ) => {
            assert_eq!(
                Some(cause),
                Some(format!("restore/{}", offload_check(*rejection))).as_deref(),
                "{id}"
            );
            let text = format!("invalid seed event at index {seq}: {}", rejection.message());
            assert_eq!(Some(text.as_str()), message, "{id}: exact message");
            return true;
        }
        (_, RestoreRefusal::Restore { seq, rejection }) => {
            let check = cause
                .strip_prefix("restore/")
                .unwrap_or_else(|| panic!("{id}: {refusal:?}"));
            let (_, expected) = SEED_CHECKS
                .iter()
                .find(|(name, _)| *name == check)
                .unwrap_or_else(|| panic!("{id}: unknown restore check {check}"));
            assert_eq!(rejection, expected, "{id}");
            *seq
        }
        _ => panic!("{id}: expected {cause}, got {refusal:?}"),
    };
    let expected = UNNAMED_SEQS
        .iter()
        .find(|(case, _)| case == id)
        .map(|(_, seq)| *seq)
        .or_else(|| message.and_then(message_seq))
        .expect("refusal message names its row");
    assert_eq!(seq, expected, "{id}: refused row");
    false
}

#[test]
fn shared_cases_restore_like_the_read_path() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut limits = BTreeSet::new();
    let mut layers = BTreeSet::new();
    let mut exact = 0;
    for case in &cases {
        let id = &case.id;
        let ts = object(&case.entry["ts"], id);
        let actual = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET);
        match case.entry.get("rust") {
            Some(rust) => {
                let rust = object(rust, id);
                match (text(&rust["outcome"], id), &actual) {
                    ("native-subset", Err(RestoreRefusal::NativeSubset { limit, .. })) => {
                        let name = text(&rust["limit"], id);
                        let (_, expected) = LIMITS
                            .iter()
                            .find(|(known, _)| *known == name)
                            .unwrap_or_else(|| panic!("{id}: unknown limit {name}"));
                        assert_eq!(limit, expected, "{id}");
                        limits.insert(name);
                    }
                    ("rejected", Err(refusal)) => {
                        assert_eq!(ts["outcome"], "rejected", "{id}");
                        let cause = text(&rust["cause"], id);
                        exact += usize::from(check_rejection(case, cause, refusal));
                        layers.insert(cause.split('/').next().expect("layer").to_owned());
                    }
                    (outcome, actual) => panic!("{id}: expected {outcome}, got {actual:?}"),
                }
            }
            None => {
                assert_eq!(
                    ts["outcome"], "restored",
                    "{id}: a rejection names its Rust cause"
                );
                let restored = actual.unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
                let closers = resolve(&case.entry["ts"]["closers"], case, &Value::Null);
                let mut expected = resolve(&case.entry["ts"], case, &closers);
                let checks = expected
                    .as_object_mut()
                    .expect("restored outcome")
                    .remove("events");
                let actual = restored_value(&restored);
                assert_eq!(actual, expected, "{id}");
                assert_eq!(
                    actual["messages"].to_string(),
                    expected["messages"].to_string(),
                    "{id}: member order"
                );
                // Restoration returns the scanned rows unchanged.
                let stored = restored.stored().rows();
                assert_eq!(stored, &case.rows[..stored.len()], "{id}: stored rows");
                for check in checks
                    .iter()
                    .flat_map(|checks| checks.as_array().expect("events"))
                {
                    let seq = index(&check["seq"], id);
                    let event = restored.stored().events().nth(seq).expect("stored event");
                    let sources: Vec<Value> = event
                        .envelope()
                        .source_event_seqs
                        .iter()
                        .flatten()
                        .map(|&seq| seq.into())
                        .collect();
                    assert_eq!(Value::Array(sources), check["sourceEventSeqs"], "{id}");
                    // The stored row keeps its logged, possibly packed, field.
                    assert_eq!(
                        restored.stored().rows()[seq]["sourceEventSeqs"],
                        case.rows[seq]["sourceEventSeqs"],
                        "{id}"
                    );
                }
            }
        }
    }
    assert_eq!(exact, OFFLOAD_MESSAGES, "exact image/offload messages");
    let all: BTreeSet<&str> = LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(limits, all, "every limit is witnessed");
    assert_eq!(
        layers,
        BTreeSet::from(["restore", "scan", "stored", "unsupported"].map(str::to_owned)),
        "every refusal layer is witnessed"
    );
}

fn case(id: &str) -> Case {
    load()
        .into_iter()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("no case {id}"))
}

/// Replay checks Session-snapshot prefixes; restoration validates the whole
/// stored log without a lossless snapshot. The same bytes differ.
#[test]
fn replay_and_restoration_differ_where_their_paths_do() {
    let replay = |id: &str| replay_requests(&case(id).log, PathPlatform::Posix, SOURCE_BUDGET);
    assert_eq!(
        replay("image-offload-tool-update-anchor"),
        Err(ReplayRefusal::Seed {
            seq: 11,
            rejection: SeedRejection::ProjectionRequired
        })
    );
    assert_eq!(
        replay("negative-zero-hook-result"),
        Err(ReplayRefusal::Seed {
            seq: 7,
            rejection: SeedRejection::LosslessJson
        })
    );
    assert_eq!(
        replay("settlement-on-last-row").map(|requests| requests.len()),
        Ok(3)
    );
    assert_eq!(
        replay("marker-on-opaque-type-after-last-settlement").map(|requests| requests.len()),
        Ok(4)
    );
    assert!(matches!(
        replay("torn-final-record"),
        Err(ReplayRefusal::Uncommitted { .. })
    ));
    assert!(matches!(
        replay("seeded-marker-last"),
        Err(ReplayRefusal::Seeded)
    ));
}

#[test]
fn stored_rows_and_closers_are_separate_and_unduplicated() {
    let restored = restore_plain_log(
        &case("end-seed-last-before-closers").log,
        PathPlatform::Posix,
        SOURCE_BUDGET,
    )
    .expect("restored");
    // The closers follow the stored end seed; the appended one is reported only.
    assert_eq!(restored.stored().rows().len(), 11);
    assert_eq!(restored.stored().rows()[10]["type"], "session/end-seed");
    let types: Vec<&Value> = restored
        .closers()
        .iter()
        .map(|closer| &closer["type"])
        .collect();
    assert_eq!(types, ["tool/result", "step/end", "turn/end"]);
    assert!(restored.end_seed_appended());
    assert_eq!(restored.stored().events().count(), 11);
}
