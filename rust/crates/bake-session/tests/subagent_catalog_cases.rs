//! Runs every shared case in `conformance/session/subagent-catalog-cases.json`
//! through `restore_plain_log` and then `subagent_catalog`, over the same
//! bytes the TypeScript spec folds. Every case must restore, and its stored
//! inherited cut and its catalog or first refused seq must meet the table's
//! hand-written `ts` values; no case is exempt. `createdAt` is JavaScript's
//! double, `-0` included, so it is compared bit for bit. Nothing here reads
//! TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    PathPlatform, RestoredLog, SubagentCatalogEntry, SubagentCatalogMode, SubagentCatalogRefusal,
    restore_plain_log, subagent_catalog,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/subagent-catalog-cases";
const ORACLE: &str = "subagentCatalogProjectionDefinition in packages/subagent/subagent/src/catalog.ts, folded from init over each case's parsed rows and their interruptedTurnClosers with the restored inherited cut";
/// The captures and their sizes; the TypeScript spec checks their SHA-256.
const LOGS: [(&str, &str, usize); 1] = [(
    "tool-call-turn",
    "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl",
    4533,
)];
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 82;
const CATALOG_TYPE: &str = "subagent/catalog";
/// The TypeScript projection appends to a chunked list; a catalog this long
/// spans more than one chunk.
const CHUNK_SPANNING_ENTRIES: usize = 65;
const SOURCE_BUDGET: usize = 64;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

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

/// A table seq or cut: a safe non-negative integer.
fn seq(value: &Value, context: &str) -> u64 {
    value
        .as_u64()
        .filter(|seq| *seq <= MAX_SAFE_INTEGER)
        .unwrap_or_else(|| panic!("{context}: expected a safe seq"))
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

struct Case {
    id: String,
    log: Vec<u8>,
    row_count: usize,
    entry: Map<String, Value>,
}

/// Apply a case's edits to its capture, as the TypeScript spec does.
fn build(entry: &Map<String, Value>, id: &str) -> (Vec<u8>, usize) {
    let name = text(&entry["log"], id);
    let (_, path, size) = LOGS
        .iter()
        .find(|(log, _, _)| *log == name)
        .unwrap_or_else(|| panic!("{id}: unknown log {name}"));
    let source = std::fs::read_to_string(repo_path(path)).expect("read capture");
    assert_eq!(source.len(), *size, "{path} changed");
    let mut rows: Vec<String> = source
        .strip_suffix('\n')
        .expect("final LF")
        .split('\n')
        .map(str::to_owned)
        .collect();
    let mut header = rows.remove(0);
    for edit in entry["edits"].as_array().expect("edits") {
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
            other => panic!("{id}: invalid edit {other:?}"),
        }
    }
    let mut log = String::new();
    for line in std::iter::once(&header).chain(&rows) {
        log.push_str(line);
        log.push('\n');
    }
    (log.into_bytes(), rows.len())
}

fn load() -> Vec<Case> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/subagent-catalog-cases.json"))
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
    let history = table["history"].as_array().expect("history");
    assert!(!history.is_empty() && history.iter().all(Value::is_string));
    assert_eq!(table["oracle"], ORACLE);
    let logs = object(&table["logs"], "logs");
    assert_eq!(logs.len(), LOGS.len());
    for (name, path, _) in LOGS {
        assert_eq!(logs[name], path);
    }
    table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|entry| {
            let entry = object(entry, "case").clone();
            let id = text(&entry["id"], "case id").to_owned();
            shape(&entry, &["id", "log", "edits", "ts"], &["note"], &id);
            assert!(
                entry.get("note").is_none_or(Value::is_string),
                "{id}: invalid note"
            );
            let (log, row_count) = build(&entry, &id);
            Case {
                id,
                log,
                row_count,
                entry,
            }
        })
        .collect()
}

/// A table `createdAt` as `JSON.parse` reads it: an integral double in
/// `[-0, 2^53 − 1]`, the range `z.number().int().nonnegative()` admits.
fn created_at(value: &Value, context: &str) -> f64 {
    let created = value
        .as_f64()
        .unwrap_or_else(|| panic!("{context}: invalid createdAt"));
    assert!(
        created.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER as f64).contains(&created),
        "{context}: invalid createdAt {created}"
    );
    created
}

/// Check a table entry's form and convert it: `label` is optional for a
/// one-shot child and required for a continuable one.
fn expected_entry(value: &Value, context: &str) -> SubagentCatalogEntry {
    let fields = object(value, context);
    let label = || {
        fields
            .get("label")
            .map(|label| text(label, context).to_owned())
    };
    let mode = match text(&fields["mode"], context) {
        "one-shot" => {
            shape(fields, &["id", "createdAt", "mode"], &["label"], context);
            SubagentCatalogMode::OneShot { label: label() }
        }
        "continuable" => {
            shape(fields, &["id", "createdAt", "mode", "label"], &[], context);
            SubagentCatalogMode::Continuable {
                label: label().expect("required label"),
            }
        }
        other => panic!("{context}: unknown mode {other}"),
    };
    SubagentCatalogEntry {
        id: text(&fields["id"], context).to_owned(),
        created_at: created_at(&fields["createdAt"], context),
        mode,
    }
}

/// Assert that the fold's entries are the table's, in order, with
/// `createdAt` bit for bit and an absent one-shot label kept absent.
fn assert_entries(actual: &[SubagentCatalogEntry], expected: &[SubagentCatalogEntry], id: &str) {
    assert_eq!(actual.len(), expected.len(), "{id}: entry count");
    for (position, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        let context = format!("{id}: entry {position}");
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
}

/// The seqs of every `subagent/catalog` event the fold visits, stored or
/// closer, inherited or own.
fn catalog_seqs(restored: &RestoredLog) -> Vec<u64> {
    let stored = restored
        .stored()
        .events()
        .filter(|event| event.envelope().event_type == CATALOG_TYPE)
        .map(|event| event.envelope().seq)
        .collect::<Vec<_>>();
    let closers = restored
        .closers()
        .iter()
        .filter(|closer| closer["type"] == CATALOG_TYPE)
        .map(|closer| closer["seq"].as_u64().expect("closer seq"));
    stored.into_iter().chain(closers).collect()
}

#[test]
fn shared_cases_fold_like_the_subagent_catalog_projection() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT, "case ids are unique");
    // What the table's cases witnessed, so a narrowed table fails.
    let mut coverage = BTreeSet::new();
    for case in &cases {
        let id = case.id.as_str();
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: every case restores, got {refusal:?}"));
        // Both arms fold the same rows: no torn tail is left out.
        assert_eq!(restored.stored().rows().len(), case.row_count, "{id}");
        assert!(restored.torn().is_none(), "{id}: torn");

        let ts = object(&case.entry["ts"], id);
        let cut = seq(&ts["inheritedEventCount"], id);
        assert_eq!(restored.stored().inherited_event_count(), cut, "{id}: cut");
        let (inherited, own): (Vec<u64>, Vec<u64>) = catalog_seqs(&restored)
            .into_iter()
            .partition(|seq| *seq < cut);
        coverage.insert(if cut > 0 { "seeded" } else { "unseeded" });
        if !restored.closers().is_empty() {
            coverage.insert("closers");
        }
        if restored.end_seed_appended() {
            coverage.insert("end-seed");
        }

        let result = subagent_catalog(&restored);
        match text(&ts["outcome"], id) {
            "catalog" => {
                shape(ts, &["outcome", "inheritedEventCount", "entries"], &[], id);
                let expected = ts["entries"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{id}: entries"))
                    .iter()
                    .enumerate()
                    .map(|(position, entry)| expected_entry(entry, &format!("{id}: ts {position}")))
                    .collect::<Vec<_>>();
                // Every own fact is kept, none deduplicated.
                assert_eq!(expected.len(), own.len(), "{id}: one entry per own event");
                let entries = result.unwrap_or_else(|refusal| panic!("{id}: refused {refusal:?}"));
                assert_entries(&entries, &expected, id);

                coverage.insert("catalog");
                if entries.is_empty() {
                    coverage.insert("empty");
                }
                if !inherited.is_empty() {
                    coverage.insert("inherited-ignored");
                }
                if entries.len() >= CHUNK_SPANNING_ENTRIES {
                    coverage.insert("chunk-spanning");
                }
                let unique: BTreeSet<&str> =
                    entries.iter().map(|entry| entry.id.as_str()).collect();
                if unique.len() < entries.len() {
                    coverage.insert("duplicate-ids");
                }
                if entries
                    .windows(2)
                    .any(|pair| pair[1].created_at < pair[0].created_at)
                {
                    coverage.insert("createdAt-unsorted");
                }
                let one_shot = entries
                    .iter()
                    .any(|entry| matches!(entry.mode, SubagentCatalogMode::OneShot { .. }));
                let continuable = entries
                    .iter()
                    .any(|entry| matches!(entry.mode, SubagentCatalogMode::Continuable { .. }));
                if one_shot && continuable {
                    coverage.insert("both-modes");
                }
                for entry in &entries {
                    coverage.insert(match &entry.mode {
                        SubagentCatalogMode::OneShot { label: None } => "one-shot",
                        SubagentCatalogMode::OneShot { label: Some(_) } => "one-shot-label",
                        SubagentCatalogMode::Continuable { .. } => "continuable",
                    });
                    if entry.created_at.to_bits() == (-0.0f64).to_bits() {
                        coverage.insert("negative-zero");
                    }
                    if entry.created_at == MAX_SAFE_INTEGER as f64 {
                        coverage.insert("max-safe");
                    }
                }
            }
            "rejected" => {
                shape(
                    ts,
                    &["outcome", "inheritedEventCount", "seq", "class"],
                    &[],
                    id,
                );
                assert_eq!(ts["class"], "ZodError", "{id}: class");
                let error_seq = seq(&ts["seq"], id);
                // Only an own catalog event can be refused.
                assert!(own.contains(&error_seq), "{id}: {error_seq} is not own");
                match result {
                    Err(SubagentCatalogRefusal { seq }) => assert_eq!(seq, error_seq, "{id}"),
                    Ok(entries) => panic!("{id}: folded {} entries", entries.len()),
                }

                coverage.insert("rejected");
                if !inherited.is_empty() {
                    coverage.insert("inherited-before-rejection");
                }
                if own.iter().any(|seq| *seq < error_seq) && own.iter().any(|seq| *seq > error_seq)
                {
                    coverage.insert("first-failure");
                }
            }
            other => panic!("{id}: unknown outcome {other}"),
        }
    }
    assert_eq!(
        coverage,
        BTreeSet::from([
            "both-modes",
            "catalog",
            "chunk-spanning",
            "closers",
            "continuable",
            "createdAt-unsorted",
            "duplicate-ids",
            "empty",
            "end-seed",
            "first-failure",
            "inherited-before-rejection",
            "inherited-ignored",
            "max-safe",
            "negative-zero",
            "one-shot",
            "one-shot-label",
            "rejected",
            "seeded",
            "unseeded",
        ])
    );
}
