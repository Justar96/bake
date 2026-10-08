//! Runs every shared case in `conformance/session/subagent-cases.json`
//! through `restore_plain_log` and then `subagent_identity` and
//! `subagent_timing`, over the same bytes the TypeScript spec folds. Every
//! case must restore, and every case's identity, full timing state, and
//! timing view must meet the table's hand-written `ts` values; no case is
//! exempt. `settledMs` is JavaScript's double, rounded above 2^53 − 1 as
//! JavaScript rounds it, so it is compared bit for bit. Nothing here reads
//! TypeScript output.

use std::collections::BTreeSet;
use std::path::PathBuf;

use bake_session::{
    PathPlatform, SubagentIdentity, SubagentTimingState, restore_plain_log, subagent_identity,
    subagent_timing,
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/subagent-cases";
const ORACLE: &str = "subagentIdentityProjectionDefinition and subagentTimingProjectionDefinition in packages/subagent/subagent/src/projection.ts, folded from init over each case's parsed rows and their interruptedTurnClosers";
/// The captures and their sizes; the TypeScript spec checks their SHA-256.
const LOGS: [(&str, &str, usize); 1] = [(
    "tool-call-turn",
    "conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl",
    4533,
)];
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 84;
/// The cases that pin JavaScript's arithmetic at and beyond 2^53 − 1:
/// exact safe totals, rounded lengths and sums, a rounded total a later
/// descriptor resets, a rounding closer, and clamped reversed spans.
const ARITHMETIC_CASES: [&str; 8] = [
    "max-safe-length",
    "max-safe-total",
    "length-over-max-safe",
    "promoted-pending-full-span",
    "total-over-max-safe-rounds",
    "overflow-then-descriptor-reset",
    "overflow-in-closer",
    "reversed-full-span-clamps",
];
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

/// A JSON integer the envelope admits as a time: safe, possibly negative.
fn time(value: &Value, context: &str) -> i64 {
    value
        .as_i64()
        .filter(|time| time.unsigned_abs() <= MAX_SAFE_INTEGER)
        .unwrap_or_else(|| panic!("{context}: expected a safe integer time"))
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
        &std::fs::read(repo_path("conformance/session/subagent-cases.json")).expect("read table"),
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

/// Check a `ts` identity's form: `null`, or `{mode, label?, seq}` with the
/// label required for a continuable child.
fn check_identity(value: &Value, id: &str) {
    if value.is_null() {
        return;
    }
    let fields = object(value, id);
    let continuable = match text(&fields["mode"], id) {
        "one-shot" => false,
        "continuable" => true,
        other => panic!("{id}: unknown mode {other}"),
    };
    if continuable {
        shape(fields, &["mode", "label", "seq"], &[], id);
    } else {
        shape(fields, &["mode", "seq"], &["label"], id);
    }
    assert!(fields.get("label").is_none_or(Value::is_string), "{id}");
    assert!(
        fields["seq"]
            .as_u64()
            .is_some_and(|seq| seq <= MAX_SAFE_INTEGER),
        "{id}: invalid seq"
    );
}

/// Check a `ts` timing state's form and the invariants every fold step keeps:
/// before a descriptor nothing is settled or active, and after one nothing is
/// pending. `settledMs` is a non-negative integral double, which may exceed
/// 2^53 − 1 where JavaScript rounded a sum.
fn check_timing(fields: &Map<String, Value>, id: &str) {
    shape(
        fields,
        &["settledMs", "descriptorSeen"],
        &["active", "pendingTurnStart"],
        id,
    );
    let settled = settled_ms(&fields["settledMs"], id);
    let seen = fields["descriptorSeen"]
        .as_bool()
        .unwrap_or_else(|| panic!("{id}: invalid descriptorSeen"));
    if let Some(active) = fields.get("active") {
        let active = object(active, id);
        shape(active, &["since", "through"], &[], id);
        time(&active["since"], id);
        time(&active["through"], id);
    }
    if let Some(pending) = fields.get("pendingTurnStart") {
        time(pending, id);
    }
    if seen {
        assert!(!fields.contains_key("pendingTurnStart"), "{id}: pending");
    } else {
        assert_eq!(settled.to_bits(), 0.0f64.to_bits(), "{id}: settled early");
        assert!(!fields.contains_key("active"), "{id}: active before one");
    }
}

/// A table `settledMs`: a finite, non-negative, integral number, as
/// `JSON.parse` reads it. `-0` is refused, since the fold's sums never make it.
fn settled_ms(value: &Value, id: &str) -> f64 {
    let settled = value
        .as_f64()
        .unwrap_or_else(|| panic!("{id}: invalid settledMs"));
    assert!(
        settled.is_finite() && settled.is_sign_positive() && settled.fract() == 0.0,
        "{id}: invalid settledMs {settled}"
    );
    settled
}

/// Assert that a fold's state or view meets the table's: `settledMs` bit for
/// bit, the other members as JSON. Each `actual` member but `settledMs` is
/// already in table form.
fn assert_timing(actual: f64, mut rest: Map<String, Value>, expected: &Value, context: &str) {
    let mut expected = object(expected, context).clone();
    let settled = settled_ms(&expected.remove("settledMs").expect("settledMs"), context);
    assert_eq!(
        actual.to_bits(),
        settled.to_bits(),
        "{context}: {actual} vs {settled}"
    );
    rest.remove("settledMs");
    assert_eq!(rest, expected, "{context}");
}

/// The view `wire.view` derives from a state: `settledMs` and `active`.
fn view_of(timing: &Map<String, Value>) -> Map<String, Value> {
    timing
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "settledMs" | "active"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// The identity view in the table's form, `null` for `None`.
fn identity_value(identity: Option<&SubagentIdentity>) -> Value {
    match identity {
        None => Value::Null,
        Some(SubagentIdentity::OneShot { seq, label }) => match label {
            Some(label) => json!({"mode": "one-shot", "label": label, "seq": seq}),
            None => json!({"mode": "one-shot", "seq": seq}),
        },
        Some(SubagentIdentity::Continuable { seq, label }) => {
            json!({"mode": "continuable", "label": label, "seq": seq})
        }
    }
}

/// The full timing state in the table's form, absent options omitted and
/// `settledMs` left to [`assert_timing`].
fn timing_value(state: &SubagentTimingState) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("descriptorSeen".to_owned(), json!(state.descriptor_seen));
    if let Some(active) = state.active {
        fields.insert(
            "active".to_owned(),
            json!({"since": active.since, "through": active.through}),
        );
    }
    if let Some(pending) = state.pending_turn_start {
        fields.insert("pendingTurnStart".to_owned(), json!(pending));
    }
    fields
}

/// What the table's cases witnessed, so a narrowed table fails.
#[derive(Default)]
struct Coverage {
    identities: BTreeSet<&'static str>,
    timings: BTreeSet<&'static str>,
    restores: BTreeSet<&'static str>,
}

#[test]
fn shared_cases_fold_like_the_subagent_projections() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), CASE_COUNT, "case ids are unique");
    for id in ARITHMETIC_CASES {
        assert!(ids.contains(id), "{id}: arithmetic case missing");
    }
    let mut coverage = Coverage::default();
    for case in &cases {
        let id = case.id.as_str();
        let restored = restore_plain_log(&case.log, PathPlatform::Posix, SOURCE_BUDGET)
            .unwrap_or_else(|refusal| panic!("{id}: every case restores, got {refusal:?}"));
        // Both arms fold the same rows: no torn tail is left out.
        assert_eq!(restored.stored().rows().len(), case.row_count, "{id}");
        if restored.stored().inherited_event_count() > 0 {
            coverage.restores.insert("inherited");
        }
        if !restored.closers().is_empty() {
            coverage.restores.insert("closers");
        }

        let ts = object(&case.entry["ts"], id);
        shape(ts, &["identity", "timing", "view"], &[], id);

        check_identity(&ts["identity"], id);
        let identity = subagent_identity(&restored);
        assert_eq!(identity_value(identity.as_ref()), ts["identity"], "{id}");
        coverage.identities.insert(match identity {
            None => "null",
            Some(SubagentIdentity::OneShot { label: None, .. }) => "one-shot",
            Some(SubagentIdentity::OneShot { label: Some(_), .. }) => "one-shot-label",
            Some(SubagentIdentity::Continuable { .. }) => "continuable",
        });

        let ts_timing = object(&ts["timing"], id);
        check_timing(ts_timing, id);
        // The table's view is its state's: `settledMs` by value, as either
        // number form, and `active` as JSON.
        assert_timing(
            settled_ms(&ts_timing["settledMs"], id),
            view_of(ts_timing),
            &ts["view"],
            &format!("{id}: ts view"),
        );

        let state = subagent_timing(&restored);
        let timing = timing_value(&state);
        assert_timing(state.settled_ms, timing.clone(), &ts["timing"], id);
        assert_timing(
            state.settled_ms,
            view_of(&timing),
            &ts["view"],
            &format!("{id}: view"),
        );
        // Session construction's end seed, stamped with the current time,
        // would move this `through`; the oracle and fold exclude it, so the
        // open interval ends at the last row.
        if state.active.is_some() && restored.end_seed_appended() {
            coverage.restores.insert("open-before-end-seed");
        }
        if state.settled_ms > MAX_SAFE_INTEGER as f64 {
            coverage.timings.insert("above-max-safe");
        }
        coverage.timings.insert(match state {
            SubagentTimingState {
                descriptor_seen: false,
                pending_turn_start: Some(_),
                ..
            } => "pending",
            SubagentTimingState {
                descriptor_seen: false,
                ..
            } => "unseen",
            SubagentTimingState {
                active: Some(_), ..
            } => "active",
            _ => "settled",
        });
    }
    assert_eq!(
        coverage.identities,
        BTreeSet::from(["continuable", "null", "one-shot", "one-shot-label"])
    );
    assert_eq!(
        coverage.timings,
        BTreeSet::from(["above-max-safe", "active", "pending", "settled", "unseen"])
    );
    assert_eq!(
        coverage.restores,
        BTreeSet::from(["closers", "inherited", "open-before-end-seed"])
    );
}
