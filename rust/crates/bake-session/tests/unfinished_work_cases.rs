//! Runs every shared case in `conformance/session/unfinished-work-cases.json`
//! through `restore_plain_log` and then `unfinished_work`, over the same row
//! prefixes the four TypeScript specs restore. Every case must restore with
//! no torn tail, and each of its five fields must meet the table's
//! hand-written `ts` value, except that a `rust` override names the inbox
//! native limit and seq Rust reports instead. `createdAt` is JavaScript's
//! double, `-0` included, so it is compared bit for bit. Nothing here reads
//! TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    InboxLimit, InboxRefusal, PathPlatform, SubagentCatalogEntry, SubagentCatalogMode,
    UnfinishedWork, restore_plain_log, unfinished_work,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/unfinished-work-cases";
const ORACLES: [(&str, &str); 4] = [
    (
        "turn",
        "interruptedTurnClosers in packages/core/session/src/repair.ts over each prefix's parsed rows: the turn and step its closers end, and each synthetic tool/result in closer order",
    ),
    (
        "compaction",
        "assertNoActiveCompaction in packages/compaction/compaction-basic/src/region.ts over each prefix's parsed rows and their interruptedTurnClosers, before any end seed Session construction appends; when it throws busy, the last compaction/start",
    ),
    (
        "children",
        "subagentCatalogProjectionDefinition in packages/subagent/subagent/src/catalog.ts, folded from init with the restored inherited cut over each prefix's parsed rows and their interruptedTurnClosers, each view entry paired with its event's seq",
    ),
    (
        "inbox",
        "inboxProjectionDefinition.apply in packages/core/agent-loop/src/inbox.ts, folded from init() over each prefix's parsed rows and their interruptedTurnClosers",
    ),
];
/// Each capture and its size; the TypeScript specs check their SHA-256.
const CAPTURES: [(&str, &str, usize); 3] = [
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
const CASE_COUNT: usize = 224;
/// Cases with an open compaction; the rest have none.
const OPEN_COMPACTIONS: usize = 9;
/// Cases whose latest bracket marker is an unmatched start that a stored end
/// seed made stale.
const STALE_COMPACTIONS: [&str; 3] = [
    "stale-then-open-compaction/18",
    "seeded-inherited-work/19",
    "seeded-inherited-work/20",
];
const SOURCE_BUDGET: usize = 64;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const INBOX_LIMITS: [(&str, InboxLimit); 4] = [
    ("target", InboxLimit::Target),
    ("count", InboxLimit::Count),
    ("inserted", InboxLimit::Inserted),
    ("message-id", InboxLimit::MessageId),
];

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

/// A table seq or row count: a safe non-negative integer.
fn count(value: &Value, context: &str) -> u64 {
    value
        .as_u64()
        .filter(|count| *count <= MAX_SAFE_INTEGER)
        .unwrap_or_else(|| panic!("{context}: expected a safe count"))
}

/// Assert that `fields` holds every `required` key and otherwise only
/// `optional` ones.
fn shape(fields: &Map<String, Value>, required: &[&str], optional: &[&str], context: &str) {
    let found = keys(fields);
    let required: BTreeSet<&str> = required.iter().copied().collect();
    let allowed: BTreeSet<&str> = required.iter().chain(optional).copied().collect();
    assert!(
        required.is_subset(&found) && found.is_subset(&allowed),
        "{context}: keys {found:?}"
    );
}

/// A log's lines, header first, and whether every prefix is a case.
struct Log {
    lines: Vec<String>,
    sweep: bool,
}

fn read_log(name: &str, value: &Value) -> Log {
    let fields = object(value, name);
    if let Some((_, path, size)) = CAPTURES.iter().find(|(capture, _, _)| *capture == name) {
        shape(fields, &["path", "sweep"], &[], name);
        assert_eq!(fields["path"], *path, "{name}");
        assert_eq!(fields["sweep"], true, "{name}");
        let source = std::fs::read_to_string(repo_path(path)).expect("read capture");
        assert_eq!(source.len(), *size, "{path} changed");
        let lines = source
            .strip_suffix('\n')
            .expect("final LF")
            .split('\n')
            .map(str::to_owned)
            .collect();
        return Log { lines, sweep: true };
    }
    shape(fields, &["derivation", "lines", "sweep"], &["source"], name);
    text(&fields["derivation"], name);
    if let Some(source) = fields.get("source") {
        text(source, name);
    }
    let sweep = fields["sweep"]
        .as_bool()
        .unwrap_or_else(|| panic!("{name}: sweep"));
    let lines: Vec<String> = fields["lines"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: lines"))
        .iter()
        .map(|line| {
            let line = text(line, name);
            assert!(!line.contains('\n'), "{name}: a line holds an LF");
            line.to_owned()
        })
        .collect();
    assert!(!lines.is_empty(), "{name}: no header");
    Log { lines, sweep }
}

struct Case {
    id: String,
    log: String,
    rows: usize,
    /// The header line and the case's rows, each ending with LF.
    bytes: Vec<u8>,
    /// The rows as parsed, by seq, for `$log` references.
    parsed: Value,
    ts: Map<String, Value>,
    /// The inbox override's limit name and seq.
    rust: Option<(String, u64)>,
}

fn parse_rust(value: &Value, id: &str) -> (String, u64) {
    let fields = object(value, id);
    shape(fields, &["inbox"], &[], id);
    let inbox = object(&fields["inbox"], id);
    shape(inbox, &["limit", "outcome", "seq"], &[], id);
    assert_eq!(inbox["outcome"], "native-subset", "{id}");
    (
        text(&inbox["limit"], id).to_owned(),
        count(&inbox["seq"], id),
    )
}

fn load() -> (Vec<(String, Log)>, Vec<Case>) {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/unfinished-work-cases.json"))
            .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    shape(
        table,
        &["cases", "history", "logs", "oracles", "schema", "version"],
        &[],
        "table",
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    let history = table["history"].as_array().expect("history");
    assert!(!history.is_empty() && history.iter().all(Value::is_string));
    let oracles = object(&table["oracles"], "oracles");
    assert_eq!(oracles.len(), ORACLES.len());
    for (field, oracle) in ORACLES {
        assert_eq!(oracles[field], oracle, "{field} oracle");
    }
    let logs: Vec<(String, Log)> = object(&table["logs"], "logs")
        .iter()
        .map(|(name, value)| (name.clone(), read_log(name, value)))
        .collect();
    let cases = table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|entry| {
            let entry = object(entry, "case");
            let id = text(&entry["id"], "case id").to_owned();
            shape(entry, &["id", "log", "rows", "ts"], &["rust", "note"], &id);
            assert!(
                entry.get("note").is_none_or(Value::is_string),
                "{id}: invalid note"
            );
            let name = text(&entry["log"], &id);
            let (_, log) = logs
                .iter()
                .find(|(log, _)| log == name)
                .unwrap_or_else(|| panic!("{id}: unknown log {name}"));
            let rows = usize::try_from(count(&entry["rows"], &id)).expect("row count");
            assert!(rows < log.lines.len(), "{id}: rows past the end");
            assert_eq!(id, format!("{name}/{rows}"), "{id}: id");
            let lines = &log.lines[..=rows];
            let mut bytes = Vec::new();
            for line in lines {
                bytes.extend_from_slice(line.as_bytes());
                bytes.push(b'\n');
            }
            let parsed = lines[1..]
                .iter()
                .map(|line| serde_json::from_str(line).expect("parse row"))
                .collect();
            Case {
                log: name.to_owned(),
                rows,
                bytes,
                parsed: Value::Array(parsed),
                ts: object(&entry["ts"], &id).clone(),
                rust: entry.get("rust").map(|rust| parse_rust(rust, &id)),
                id,
            }
        })
        .collect();
    (logs, cases)
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

/// Replace each `{ "$log": pointer }` with the value it names in the case's
/// own rows.
fn resolve(value: &Value, case: &Case) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|item| resolve(item, case)).collect()),
        Value::Object(fields) => {
            if fields.keys().any(|key| key.starts_with('$')) {
                assert_eq!(keys(fields), BTreeSet::from(["$log"]), "{}", case.id);
                let pointer = text(&fields["$log"], &case.id);
                return at(&case.parsed, pointer, &case.id).clone();
            }
            Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| (key.clone(), resolve(item, case)))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

fn check_turn_and_tools(case: &Case, work: &UnfinishedWork) {
    let id = &case.id;
    let turn = work.turn.map_or(
        Value::Null,
        |turn| json!({"turn": turn.turn, "step": turn.step}),
    );
    assert_eq!(turn, resolve(&case.ts["turn"], case), "{id}: turn");
    let tools: Vec<Value> = work
        .tools
        .iter()
        .map(|tool| {
            json!({
                "callId": tool.call_id,
                "closerSeq": tool.closer_seq,
                "step": tool.step,
                "code": tool.code(),
                "callSeq": tool.call_seq,
            })
        })
        .collect();
    assert_eq!(
        Value::Array(tools),
        resolve(&case.ts["tools"], case),
        "{id}: tools"
    );
}

fn check_compaction(case: &Case, work: &UnfinishedWork) {
    let compaction = work.compaction.as_ref().map_or(
        Value::Null,
        |compaction| json!({"startSeq": compaction.start_seq, "data": compaction.data}),
    );
    assert_eq!(
        compaction,
        resolve(&case.ts["compaction"], case),
        "{}: compaction",
        case.id
    );
}

/// A table child: its seq and entry, `label` optional for a one-shot child
/// and required for a continuable one, and `createdAt` an integral double in
/// `[-0, 2^53 − 1]`.
fn expected_child(value: &Value, context: &str) -> (u64, SubagentCatalogEntry) {
    let fields = object(value, context);
    let label = || {
        fields
            .get("label")
            .map(|label| text(label, context).to_owned())
    };
    let mode = match text(&fields["mode"], context) {
        "one-shot" => {
            shape(
                fields,
                &["seq", "id", "createdAt", "mode"],
                &["label"],
                context,
            );
            SubagentCatalogMode::OneShot { label: label() }
        }
        "continuable" => {
            shape(
                fields,
                &["seq", "id", "createdAt", "mode", "label"],
                &[],
                context,
            );
            SubagentCatalogMode::Continuable {
                label: label().expect("required label"),
            }
        }
        other => panic!("{context}: unknown mode {other}"),
    };
    let created_at = fields["createdAt"]
        .as_f64()
        .unwrap_or_else(|| panic!("{context}: invalid createdAt"));
    assert!(
        created_at.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER as f64).contains(&created_at),
        "{context}: invalid createdAt {created_at}"
    );
    let entry = SubagentCatalogEntry {
        id: text(&fields["id"], context).to_owned(),
        created_at,
        mode,
    };
    (count(&fields["seq"], context), entry)
}

/// Check the children and return the table's, for coverage.
fn check_children(case: &Case, work: &UnfinishedWork) -> Option<Vec<(u64, SubagentCatalogEntry)>> {
    let id = &case.id;
    let expected = &case.ts["children"];
    if let Some(refusal) = expected.get("refusal") {
        shape(object(expected, id), &["refusal"], &[], id);
        shape(object(refusal, id), &["seq"], &[], id);
        let error_seq = count(&refusal["seq"], id);
        match &work.children {
            Err(refusal) => assert_eq!(refusal.seq, error_seq, "{id}: children refusal"),
            Ok(children) => panic!("{id}: listed {} children", children.len()),
        }
        return None;
    }
    let expected: Vec<(u64, SubagentCatalogEntry)> = expected
        .as_array()
        .unwrap_or_else(|| panic!("{id}: children"))
        .iter()
        .enumerate()
        .map(|(position, child)| expected_child(child, &format!("{id}: child {position}")))
        .collect();
    let actual = work
        .children
        .as_ref()
        .unwrap_or_else(|refusal| panic!("{id}: children refused {refusal:?}"));
    assert_eq!(actual.len(), expected.len(), "{id}: child count");
    for (position, ((seq, actual), (expected_seq, expected))) in
        actual.iter().zip(&expected).enumerate()
    {
        let context = format!("{id}: child {position}");
        assert_eq!(seq, expected_seq, "{context}: seq");
        assert_eq!(actual.id, expected.id, "{context}: id");
        assert_eq!(
            actual.created_at.to_bits(),
            expected.created_at.to_bits(),
            "{context}: createdAt {} vs {}",
            actual.created_at,
            expected.created_at
        );
        assert_eq!(actual.mode, expected.mode, "{context}: mode");
    }
    Some(expected)
}

/// Check the inbox and return the witnessed limit, if any.
fn check_inbox(case: &Case, work: &UnfinishedWork) -> Option<&'static str> {
    let id = &case.id;
    if let Some((name, seq)) = &case.rust {
        let (known, limit) = INBOX_LIMITS
            .iter()
            .find(|(known, _)| known == name)
            .unwrap_or_else(|| panic!("{id}: unknown inbox limit {name}"));
        assert_eq!(
            work.inbox,
            Err(InboxRefusal::NativeSubset {
                seq: *seq,
                limit: *limit
            }),
            "{id}"
        );
        return Some(known);
    }
    let actual = match &work.inbox {
        Ok(inbox) => json!({"nextTurn": inbox.next_turn, "nextStep": inbox.next_step}),
        Err(refusal) => json!({
            "refusal": refusal.message().unwrap_or_else(|| panic!("{id}: {refusal:?}")),
        }),
    };
    assert_eq!(actual, resolve(&case.ts["inbox"], case), "{id}: inbox");
    None
}

/// Whether the latest bracket marker among the case's rows is a start.
fn unmatched_start(case: &Case) -> bool {
    case.parsed
        .as_array()
        .expect("rows")
        .iter()
        .rev()
        .map(|row| &row["type"])
        .find(|kind| *kind == "compaction/start" || *kind == "compaction/end")
        .is_some_and(|kind| kind == "compaction/start")
}

#[test]
fn shared_cases_project_like_the_typescript_folds() {
    let (logs, cases) = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT, "case ids are unique");
    for (name, log) in &logs {
        if log.sweep {
            let swept: Vec<usize> = cases
                .iter()
                .filter(|case| case.log == *name)
                .map(|case| case.rows)
                .collect();
            let all: Vec<usize> = (0..log.lines.len()).collect();
            assert_eq!(swept, all, "{name}: every prefix once");
        }
    }
    let mut inbox_limits = BTreeSet::new();
    let mut stale = Vec::new();
    let mut open_compactions = 0;
    // What the table's cases witnessed, so a narrowed table fails.
    let mut coverage = BTreeSet::new();
    for case in &cases {
        let id = case.id.as_str();
        let restored = restore_plain_log(&case.bytes, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: every case restores, got {refusal:?}"));
        assert_eq!(restored.stored().rows().len(), case.rows, "{id}");
        assert!(restored.torn().is_none(), "{id}: torn");
        let work = unfinished_work(&restored);

        check_turn_and_tools(case, &work);
        check_compaction(case, &work);
        let children = check_children(case, &work);
        inbox_limits.extend(check_inbox(case, &work));

        match work.turn {
            Some(turn) if turn.step.is_some() => coverage.insert("open-step"),
            Some(_) => coverage.insert("open-turn-without-step"),
            None => coverage.insert("no-open-turn"),
        };
        for tool in &work.tools {
            coverage.insert(tool.code());
        }
        if work.compaction.is_some() {
            open_compactions += 1;
        } else if unmatched_start(case) {
            stale.push(id);
        }
        match &children {
            None => {
                coverage.insert("children-refusal");
            }
            Some(children) => {
                let unique: BTreeSet<&str> = children
                    .iter()
                    .map(|(_, entry)| entry.id.as_str())
                    .collect();
                if unique.len() < children.len() {
                    coverage.insert("duplicate-ids");
                }
                if children
                    .iter()
                    .any(|(_, entry)| entry.created_at.to_bits() == (-0.0f64).to_bits())
                {
                    coverage.insert("negative-zero");
                }
                let cut = restored.stored().inherited_event_count();
                let inherited_catalog = case.parsed.as_array().expect("rows").iter().any(|row| {
                    row["type"] == "subagent/catalog"
                        && row["seq"].as_u64().is_some_and(|seq| seq < cut)
                });
                if !children.is_empty() && inherited_catalog {
                    coverage.insert("inherited-catalog-ignored");
                }
            }
        }
        match &work.inbox {
            Err(InboxRefusal::InvalidSplice { .. }) => {
                coverage.insert("inbox-refusal");
            }
            Ok(inbox) if !inbox.next_turn.is_empty() || !inbox.next_step.is_empty() => {
                coverage.insert("inbox-pending");
            }
            _ => {}
        }
        let all_open = work.turn.is_some()
            && !work.tools.is_empty()
            && work.compaction.is_some()
            && children.is_some_and(|children| !children.is_empty())
            && work
                .inbox
                .as_ref()
                .is_ok_and(|inbox| !inbox.next_turn.is_empty() || !inbox.next_step.is_empty());
        if all_open {
            coverage.insert("all-open");
        }
    }
    assert_eq!(open_compactions, OPEN_COMPACTIONS, "open compactions");
    assert_eq!(stale, STALE_COMPACTIONS, "stale compactions");
    let all: BTreeSet<&str> = INBOX_LIMITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(inbox_limits, all, "every inbox limit is witnessed");
    assert_eq!(
        coverage,
        BTreeSet::from([
            "TOOL_NOT_STARTED",
            "TOOL_OUTCOME_UNKNOWN",
            "all-open",
            "children-refusal",
            "duplicate-ids",
            "inbox-pending",
            "inbox-refusal",
            "inherited-catalog-ignored",
            "negative-zero",
            "no-open-turn",
            "open-step",
            "open-turn-without-step",
        ])
    );
}
