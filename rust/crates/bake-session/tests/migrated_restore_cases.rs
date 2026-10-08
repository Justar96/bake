//! Runs every shared case in `conformance/session/migrated-restore-cases.json`
//! through the released format chain to v3, `migrate_v2_rows` for a v2
//! Session or `decode_v0_v1_items` and `migrate_released_history` for a v0 or
//! v1 one, which must succeed, and then `restore_migrated`, on both path
//! platforms. A case's expected outcome is its `rust` override when present,
//! otherwise the hand-written restored state, with messages also compared as
//! serialized text, since `Value` equality ignores member order. The
//! expectations were written from the TypeScript sources; nothing here reads
//! TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    HeaderOrigin, MigratedRestoreRefusal, MigratedV2, PathPlatform, RestoreRefusal, RestoredLog,
    SessionHeader, Unsupported, V1CodecRecovery, V1CodecVersion, decode_v0_v1_items,
    migrate_released_history, migrate_v2_rows, restore_migrated,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/migrated-restore-cases";
const ORACLE: &str = "JsonlSessionPersistence({compression: 'none'}) with session.v<N>.jsonl in its Session directory, readColdSessionLog, then Session.fromRestore(..., currentSessionMessageProjections)";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 15;
/// The migration's source budget, and the scan's unless a case sets one.
const SOURCE_BUDGET: usize = 10_000;
/// The limit names the table may use, each witnessed. A restoration limit
/// passes through as `restore/<name>` and is witnessed by the restore table.
const LIMITS: [&str; 2] = ["encode", "scan"];
const RESTORED_KEYS: [&str; 10] = [
    "outcome",
    "header",
    "inheritedEventCount",
    "events",
    "closers",
    "endSeedAppended",
    "messages",
    "requestHeader",
    "toolHistory",
    "requestContext",
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn object<'a>(value: &'a Value, context: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected an object, got {value}"))
}

fn keys(fields: &Map<String, Value>) -> BTreeSet<&str> {
    fields.keys().map(String::as_str).collect()
}

fn text<'a>(value: &'a Value, context: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a string"))
}

fn parse_text(value: &Value, context: &str) -> Value {
    serde_json::from_str(text(value, context)).unwrap_or_else(|error| panic!("{context}: {error}"))
}

/// What Rust must report for a case.
enum Expected {
    Restored(Value),
    Limit(String),
    Refused(String),
    OutsideDomain,
}

struct Case {
    id: String,
    version: u64,
    header: Value,
    rows: Vec<Value>,
    source_budget: usize,
    expect: Expected,
}

fn load() -> Vec<Case> {
    let text_value =
        std::fs::read_to_string(repo_path("conformance/session/migrated-restore-cases.json"))
            .expect("read migrated-restore-cases.json");
    let table: Value =
        serde_json::from_str(&text_value).expect("parse migrated-restore-cases.json");
    let fields = object(&table, "table");
    assert_eq!(
        keys(fields),
        BTreeSet::from(["schema", "version", "oracle", "history", "cases"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], ORACLE);
    assert!(
        table["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    let allowed = BTreeSet::from([
        "id",
        "version",
        "header",
        "rows",
        "sourceBudget",
        "ts",
        "rust",
        "note",
    ]);
    table["cases"]
        .as_array()
        .expect("cases must be an array")
        .iter()
        .map(|entry| {
            let fields = object(entry, "case");
            let id = text(&entry["id"], "case id").to_owned();
            assert!(keys(fields).is_subset(&allowed), "{id}: unknown keys");
            assert!(
                entry.get("note").is_none_or(Value::is_string),
                "{id}: invalid note"
            );
            let version = entry["version"]
                .as_u64()
                .filter(|version| *version <= 2)
                .unwrap_or_else(|| panic!("{id}: version must be 0, 1, or 2"));
            let rows = entry["rows"]
                .as_array()
                .unwrap_or_else(|| panic!("{id}: rows must be an array"))
                .iter()
                .enumerate()
                .map(|(index, row)| parse_text(row, &format!("{id} row {index}")))
                .collect();
            let source_budget = entry.get("sourceBudget").map_or(SOURCE_BUDGET, |budget| {
                budget
                    .as_u64()
                    .and_then(|budget| usize::try_from(budget).ok())
                    .unwrap_or_else(|| panic!("{id}: invalid sourceBudget"))
            });
            let ts = object(&entry["ts"], &id);
            let restored = match text(&ts["outcome"], &id) {
                "restored" => {
                    assert_eq!(keys(ts), BTreeSet::from(RESTORED_KEYS), "{id}: ts keys");
                    true
                }
                "rejected" => {
                    assert_eq!(
                        keys(ts),
                        BTreeSet::from(["outcome", "class", "message"]),
                        "{id}: ts keys"
                    );
                    false
                }
                other => panic!("{id}: invalid ts outcome {other}"),
            };
            let expect = match entry.get("rust") {
                None => {
                    assert!(restored, "{id}: a rejection names its Rust outcome");
                    Expected::Restored(entry["ts"].clone())
                }
                Some(rust) => {
                    let rust = object(rust, &id);
                    match text(&rust["outcome"], &id) {
                        "native-subset" => {
                            assert_eq!(keys(rust), BTreeSet::from(["outcome", "limit"]), "{id}");
                            let limit = text(&rust["limit"], &id);
                            assert!(LIMITS.contains(&limit), "{id}: unlisted limit {limit}");
                            Expected::Limit(limit.to_owned())
                        }
                        "refused" => {
                            assert_eq!(keys(rust), BTreeSet::from(["outcome", "cause"]), "{id}");
                            assert!(!restored, "{id}: a Rust refusal claims a TypeScript one");
                            Expected::Refused(text(&rust["cause"], &id).to_owned())
                        }
                        "outside-domain" => {
                            assert_eq!(keys(rust), BTreeSet::from(["outcome"]), "{id}");
                            assert!(!restored, "{id}: only a TypeScript refusal is outside");
                            Expected::OutsideDomain
                        }
                        other => panic!("{id}: invalid rust outcome {other}"),
                    }
                }
            };
            Case {
                version,
                header: parse_text(&entry["header"], &format!("{id} header")),
                rows,
                source_budget,
                expect,
                id,
            }
        })
        .collect()
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

/// The stored events as TypeScript holds them: each scanned row with its
/// `sourceEventSeqs` expanded.
fn stored_events(restored: &RestoredLog) -> Vec<Value> {
    let stored = restored.stored();
    stored
        .rows()
        .iter()
        .zip(stored.events())
        .map(|(row, event)| {
            let mut row = row.clone();
            if let Some(sources) = &event.envelope().source_event_seqs {
                row["sourceEventSeqs"] = json!(sources);
            }
            row
        })
        .collect()
}

fn restored_value(restored: &RestoredLog) -> Value {
    let stored = restored.stored();
    json!({
        "outcome": "restored",
        "header": header_meta(stored.header()),
        "inheritedEventCount": stored.inherited_event_count(),
        "events": stored_events(restored),
        "closers": restored.closers(),
        "endSeedAppended": restored.end_seed_appended(),
        "messages": restored.messages(),
        "requestHeader": restored.request_header(),
        "toolHistory": restored.tool_history(),
        "requestContext": restored.request_context(),
    })
}

/// `UpperCamel` as `kebab-case`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        if character.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('-');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// The table's name for a restoration refusal's layer and check.
fn cause(refusal: &RestoreRefusal) -> String {
    match refusal {
        RestoreRefusal::Unsupported {
            cause: Unsupported::UnknownType,
            ..
        } => "unsupported/unknown-type".to_owned(),
        RestoreRefusal::Unsupported {
            cause: Unsupported::FallbackHeader,
            ..
        } => "unsupported/fallback-header".to_owned(),
        RestoreRefusal::Stored { rejection, .. } => {
            format!("stored/{}", kebab(&format!("{rejection:?}")))
        }
        RestoreRefusal::Restore { rejection, .. } => {
            format!("restore/{}", kebab(&format!("{rejection:?}")))
        }
        other => format!("{other:?}"),
    }
}

fn migrate(case: &Case, platform: PathPlatform) -> Result<MigratedV2, String> {
    if case.version == 2 {
        return migrate_v2_rows(&case.header, &case.rows, platform, SOURCE_BUDGET)
            .map_err(|refusal| format!("v2 migration: {refusal:?}"));
    }
    let version = if case.version == 0 {
        V1CodecVersion::V0
    } else {
        V1CodecVersion::V1
    };
    let decoded = decode_v0_v1_items(
        &case.header,
        &case.rows,
        version,
        V1CodecRecovery::Strict,
        platform,
        SOURCE_BUDGET,
    )
    .map_err(|refusal| format!("decode: {refusal:?}"))?;
    migrate_released_history(&decoded).map_err(|refusal| format!("history: {refusal:?}"))
}

/// The first mismatch of one case on one platform, if any.
fn check(case: &Case, platform: PathPlatform) -> Option<String> {
    let id = &case.id;
    let migrated = match migrate(case, platform) {
        Ok(migrated) => migrated,
        Err(failure) => return Some(format!("{id}: does not migrate: {failure}")),
    };
    let actual = restore_migrated(&migrated, platform, case.source_budget);
    match (&case.expect, actual) {
        (Expected::Restored(expected), Ok(restored)) => {
            if restored.torn().is_some() {
                return Some(format!("{id}: an encoded log has no torn tail"));
            }
            let actual = restored_value(&restored);
            if actual != *expected {
                return Some(format!("{id}: got {actual}, expected {expected}"));
            }
            // `Value` equality ignores member order; the text does not.
            let order = |value: &Value| serde_json::to_string(&value["messages"]).ok();
            (order(&actual) != order(expected)).then(|| format!("{id}: message member order"))
        }
        (Expected::Limit(name), Err(MigratedRestoreRefusal::NativeSubset(limit)))
            if limit.name() == *name =>
        {
            None
        }
        (Expected::Refused(name), Err(MigratedRestoreRefusal::Refused(refusal)))
            if cause(&refusal) == *name =>
        {
            None
        }
        // Nothing is claimed; the case only has to run.
        (Expected::OutsideDomain, _) => None,
        (Expected::Restored(_), actual) => Some(format!("{id}: expected restored, got {actual:?}")),
        (Expected::Limit(name), actual) => Some(format!("{id}: expected {name}, got {actual:?}")),
        (Expected::Refused(name), actual) => Some(format!("{id}: expected {name}, got {actual:?}")),
    }
}

#[test]
fn table_pins_its_size_and_limits() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT, "case ids must be unique");
    let witnessed: BTreeSet<&str> = cases
        .iter()
        .filter_map(|case| match &case.expect {
            Expected::Limit(limit) => Some(limit.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        witnessed,
        BTreeSet::from(LIMITS),
        "every limit is witnessed"
    );
    assert!(
        cases
            .iter()
            .any(|case| matches!(case.expect, Expected::Refused(_))),
        "a Rust refusal is witnessed"
    );
    assert!(
        cases
            .iter()
            .any(|case| matches!(case.expect, Expected::OutsideDomain)),
        "an outside-domain case is witnessed"
    );
}

#[test]
fn shared_cases_restore_like_the_read_path_on_both_platforms() {
    let mut failures = Vec::new();
    for case in load() {
        for platform in [PathPlatform::Posix, PathPlatform::Win32] {
            if let Some(failure) = check(&case, platform) {
                failures.push(format!("{platform:?} {failure}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
