//! Runs every shared case in `conformance/session/prompt-admission-cases.json`
//! through `restore_plain_log`, then `system_prompt_commits`,
//! `content_generation`, `tools_changed`, and `starts_request_series`, over
//! the same bytes the TypeScript spec builds. Every case without a `rust`
//! marker restores with no torn tail and checks the hand-written TypeScript
//! outcome; a marked case must be refused with the inherited restore limit it
//! names, which must be witnessed. Nothing here reads TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    PathPlatform, PromptDecision, PromptIntent, RestoreLimit, RestoreRefusal, RestoredLog,
    SystemPromptCommit, content_generation, restore_plain_log, starts_request_series,
    system_prompt_commits, tools_changed,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/prompt-admission-cases";
const ORACLE: &str = "SystemPromptProjection.project, session.surface.contentGeneration, and headerEquals(baseline, canonicalHeader({...baseline, tools})) over the Session restorePlainLog(log) restores in packages/core/agent-loop/tests/prompt-admission-conformance.spec.ts; series is that spec's composition of the startsSeries disjunction in agent.ts, not production code";
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
const CASE_COUNT: usize = 34;
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

fn flag(value: &Value, context: &str) -> bool {
    value
        .as_bool()
        .unwrap_or_else(|| panic!("{context}: expected a boolean"))
}

fn count(value: &Value, context: &str) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("{context}: expected a count"))
}

fn index(value: &Value, context: &str) -> usize {
    usize::try_from(count(value, context)).expect("index")
}

fn list<'a>(value: &'a Value, context: &str) -> &'a [Value] {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{context}: expected an array"))
}

struct Case {
    id: String,
    log: Vec<u8>,
    row_count: usize,
    /// The edited rows as parsed, for `$log` references.
    rows: Value,
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
    for edit in list(&entry["edits"], id) {
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
            ["row", "text"] => {
                let row = index(&edit["row"], id);
                assert!(row < rows.len(), "{id}: no row {row}");
                rows[row] = line("text");
            }
            ["find", "replace", "row"] => {
                let row = index(&edit["row"], id);
                assert!(row < rows.len(), "{id}: no row {row}");
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
    (log.into_bytes(), rows)
}

fn load() -> Vec<Case> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/prompt-admission-cases.json"))
            .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "logs", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    assert!(
        list(&table["history"], "history")
            .iter()
            .all(Value::is_string)
    );
    let logs = object(&table["logs"], "logs");
    assert_eq!(logs.len(), LOGS.len());
    for (name, path, _) in LOGS {
        assert_eq!(logs[name], path);
    }
    list(&table["cases"], "cases")
        .iter()
        .map(|entry| {
            let entry = object(entry, "case").clone();
            let id = text(&entry["id"], "case id").to_owned();
            let allowed = BTreeSet::from(["id", "log", "edits", "ts", "rust", "note"]);
            assert!(keys(&entry).is_subset(&allowed), "{id}: unknown keys");
            assert!(entry.contains_key("ts"), "{id}: no expectation");
            if let Some(note) = entry.get("note") {
                text(note, &id);
            }
            let (log, rows) = build(&entry, &id);
            let row_count = rows.len();
            let rows = rows
                .iter()
                .map(|row| serde_json::from_str(row).unwrap_or(Value::Null))
                .collect();
            Case {
                id,
                log,
                row_count,
                rows: Value::Array(rows),
                entry,
            }
        })
        .collect()
}

/// Resolve a JSON pointer without `~` escapes, refusing a missing member and
/// an array step that is not a canonical decimal index, such as `01`.
fn at<'a>(root: &'a Value, pointer: &str, id: &str) -> &'a Value {
    assert!(
        pointer.starts_with('/') && !pointer.contains('~'),
        "{id}: unsupported pointer {pointer}"
    );
    pointer.split('/').skip(1).fold(root, |node, key| {
        let next = match node {
            Value::Array(items) => key
                .parse::<usize>()
                .ok()
                .filter(|index| index.to_string() == key)
                .and_then(|index| items.get(index)),
            Value::Object(fields) => fields.get(key),
            _ => None,
        };
        next.unwrap_or_else(|| panic!("{id}: {pointer} does not exist"))
    })
}

/// Replace each `{ "$log": pointer }` with the value it names in the case's
/// edited rows.
fn resolve(value: &Value, case: &Case) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|item| resolve(item, case)).collect()),
        Value::Object(fields) => {
            if fields.keys().any(|key| key.contains('$')) {
                assert_eq!(
                    keys(fields),
                    BTreeSet::from(["$log"]),
                    "{}: invalid reference",
                    case.id
                );
                return at(&case.rows, text(&fields["$log"], &case.id), &case.id).clone();
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

/// A commit in the table's form.
fn commit_value(commit: &SystemPromptCommit) -> Value {
    let intent = match commit.intent {
        PromptIntent::Append => json!("append"),
        PromptIntent::Replace { seq } => json!({ "replace": seq }),
    };
    json!({ "text": commit.text, "intent": intent })
}

fn tools(value: &Value, case: &Case) -> Vec<Value> {
    list(&resolve(value, case), &case.id).to_vec()
}

fn check(case: &Case, restored: &RestoredLog) {
    let id = &case.id;
    let ts = object(&case.entry["ts"], id);
    assert_eq!(
        keys(ts),
        BTreeSet::from(["contentGeneration", "prompts", "series", "toolsChanged"]),
        "{id}"
    );
    assert_eq!(
        content_generation(restored),
        count(&ts["contentGeneration"], id),
        "{id}: content generation"
    );
    for (position, prompt) in list(&ts["prompts"], id).iter().enumerate() {
        let prompt = object(prompt, id);
        assert_eq!(
            keys(prompt),
            BTreeSet::from(["commits", "inHistory", "rendered", "startsSeries"]),
            "{id}"
        );
        let decision = PromptDecision {
            in_history: flag(&prompt["inHistory"], id),
            starts_series: flag(&prompt["startsSeries"], id),
        };
        let actual: Vec<Value> =
            system_prompt_commits(restored, text(&prompt["rendered"], id), decision)
                .iter()
                .map(commit_value)
                .collect();
        assert_eq!(
            Value::Array(actual),
            prompt["commits"],
            "{id}: prompt {position}"
        );
    }
    for (position, query) in list(&ts["toolsChanged"], id).iter().enumerate() {
        let query = object(query, id);
        assert_eq!(keys(query), BTreeSet::from(["changed", "tools"]), "{id}");
        assert_eq!(
            tools_changed(restored, &tools(&query["tools"], case)),
            flag(&query["changed"], id),
            "{id}: tools {position}"
        );
    }
    for (position, query) in list(&ts["series"], id).iter().enumerate() {
        let query = object(query, id);
        assert_eq!(
            keys(query),
            BTreeSet::from([
                "declared",
                "generationAtLastRequest",
                "startsSeries",
                "toolUpdateRoute",
                "tools"
            ]),
            "{id}"
        );
        let actual = starts_request_series(
            flag(&query["declared"], id),
            count(&query["generationAtLastRequest"], id),
            restored,
            flag(&query["toolUpdateRoute"], id),
            &tools(&query["tools"], case),
        );
        assert_eq!(
            actual,
            flag(&query["startsSeries"], id),
            "{id}: series {position}"
        );
    }
}

#[test]
fn shared_cases_admit_prompts_like_the_agent_loop() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut witnessed = false;
    for case in &cases {
        let id = &case.id;
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET);
        if let Some(rust) = case.entry.get("rust") {
            let rust = object(rust, id);
            assert_eq!(
                keys(rust),
                BTreeSet::from(["limit", "outcome", "seq"]),
                "{id}"
            );
            assert_eq!(rust["outcome"], "native-subset", "{id}");
            assert_eq!(rust["limit"], "number", "{id}");
            let seq = count(&rust["seq"], id);
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
        let restored = restored
            .unwrap_or_else(|refusal| panic!("{id}: unmarked cases restore, got {refusal:?}"));
        assert_eq!(restored.stored().rows().len(), case.row_count, "{id}");
        check(case, &restored);
    }
    assert!(witnessed, "the number limit is witnessed");
}

fn reference_case() -> Case {
    Case {
        id: "reference".to_owned(),
        log: Vec::new(),
        row_count: 1,
        rows: json!([{"data": [1]}]),
        entry: Map::new(),
    }
}

#[test]
fn references_resolve_canonical_indexes() {
    assert_eq!(
        resolve(&json!({"$log": "/0/data/0"}), &reference_case()),
        json!(1)
    );
}

#[test]
#[should_panic(expected = "does not exist")]
fn references_refuse_noncanonical_indexes() {
    resolve(&json!({"$log": "/00/data"}), &reference_case());
}

#[test]
#[should_panic(expected = "invalid reference")]
fn references_refuse_other_dollar_keys() {
    resolve(&json!({"log$": "/0"}), &reference_case());
}
