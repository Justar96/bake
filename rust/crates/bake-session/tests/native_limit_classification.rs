//! Checks `docs/roadmap/rust-0.4/scope-02/native-limits.json`, the D21
//! classification of every native limit, against the sources it classifies:
//! every variant of an enum named `*Limit` or `*Coercion` in this crate's
//! `src/`, and every conformance case expectation that names a native limit,
//! must each belong to exactly one classified limit, with matching case
//! counts. A new limit-enum variant or limit case therefore fails here until
//! it is classified. String-named stage limits are not read from the source,
//! so a new one that no case witnesses is classified by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

const SCHEMA: &str = "bake/rust-migration/native-limit-classification";
const VERSION: u64 = 1;
const CLASSIFICATION: &str = "docs/roadmap/rust-0.4/scope-02/native-limits.json";
const CLASSIFICATIONS: [&str; 3] = ["unreachable", "reachable-gap", "undecided"];
const KINDS: [&str; 5] = [
    "format",
    "resource",
    "filesystem",
    "pass-through",
    "invariant",
];
const ENTRY_KEYS: [&str; 9] = [
    "cases",
    "classification",
    "crate",
    "evidence",
    "followUp",
    "id",
    "kind",
    "stageNames",
    "variants",
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    // Tables may hold lone surrogates, which only `parse_json` reads.
    bake_session::parse_json(&text).unwrap_or_else(|error| panic!("{path:?}: {error:?}"))
}

fn object<'a>(value: &'a Value, context: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected an object"))
}

fn text<'a>(value: &'a Value, context: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a string"))
}

fn array<'a>(value: &'a Value, context: &str) -> &'a [Value] {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{context}: expected an array"))
}

/// Files under `dir` with the given extension, sorted, as paths relative to
/// `dir` joined with `/`.
fn files(dir: &Path, extension: &str) -> Vec<(String, PathBuf)> {
    fn walk(root: &Path, dir: &Path, extension: &str, out: &mut Vec<(String, PathBuf)>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("{dir:?}: {error}"))
            .map(|entry| entry.expect("directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(root, &path, extension, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
                let relative = path
                    .strip_prefix(root)
                    .expect("walked path is under its root")
                    .components()
                    .map(|part| part.as_os_str().to_str().expect("UTF-8 path"))
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((relative, path));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, extension, &mut out);
    out
}

/// Every `Enum::Variant` of a `pub enum` named `*Limit` or `*Coercion`, read
/// from rustfmt-formatted source: variants sit at one indentation level.
fn source_variants() -> BTreeSet<String> {
    let mut variants = BTreeSet::new();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for (relative, path) in files(&src, "rs") {
        let source = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
        let mut current: Option<String> = None;
        for line in source.lines() {
            if let Some(name) = &current {
                if line == "}" {
                    current = None;
                    continue;
                }
                let Some(body) = line.strip_prefix("    ") else {
                    continue;
                };
                if !body.starts_with(|c: char| c.is_ascii_uppercase()) {
                    continue;
                }
                let variant: String = body
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                assert!(
                    variants.insert(format!("{name}::{variant}")),
                    "{relative}: {name}::{variant} appears twice"
                );
                continue;
            }
            let Some(rest) = line.strip_prefix("pub enum ") else {
                continue;
            };
            let Some(name) = rest.strip_suffix(" {") else {
                continue;
            };
            if name.ends_with("Limit") || name.ends_with("Coercion") {
                current = Some(name.to_owned());
            }
        }
        assert!(current.is_none(), "{relative}: unterminated limit enum");
    }
    variants
}

/// Counts every native-limit expectation in a conformance case table by name: a
/// string `limit` member, prefixed with `<scope>:` when its object sits under
/// a key other than `rust`, and a `kind: "native-limit"` refusal as
/// `<stage>/<reason>`, with an empty reason when it is `null`.
fn collect_limits(value: &Value, parent: &str, context: &str, out: &mut BTreeMap<String, u64>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_limits(item, parent, context, out);
            }
        }
        Value::Object(fields) => {
            if let Some(limit) = fields.get("limit") {
                let limit = text(limit, &format!("{context}: limit"));
                let name = if parent == "rust" {
                    limit.to_owned()
                } else {
                    format!("{parent}:{limit}")
                };
                *out.entry(name).or_default() += 1;
            }
            if fields.get("kind").and_then(Value::as_str) == Some("native-limit") {
                let stage = text(&fields["stage"], &format!("{context}: stage"));
                let reason = match &fields["reason"] {
                    Value::Null => "",
                    reason => text(reason, &format!("{context}: reason")),
                };
                *out.entry(format!("{stage}/{reason}")).or_default() += 1;
            }
            for (key, field) in fields {
                collect_limits(field, key, context, out);
            }
        }
        _ => {}
    }
}

fn conformance_limits() -> BTreeMap<(String, String), u64> {
    let mut limits = BTreeMap::new();
    for (table, path) in files(&repo_path("conformance"), "json") {
        // Shared case tables; `invalid/` holds deliberately malformed
        // synthetic-runner documents, and fixtures carry no expectations.
        if !table.ends_with("-cases.json") {
            continue;
        }
        let mut names = BTreeMap::new();
        collect_limits(&read_json(&path), "", &table, &mut names);
        for (name, count) in names {
            limits.insert((table.clone(), name), count);
        }
    }
    limits
}

/// Whether `evidence` cites a source line as `<file>.<ext>:<digit>` or a
/// committed schema snapshot.
fn cites_source(evidence: &str) -> bool {
    [".ts:", ".rs:", ".js:"].iter().any(|marker| {
        evidence
            .match_indices(marker)
            .any(|(at, _)| evidence[at + marker.len()..].starts_with(|c: char| c.is_ascii_digit()))
    }) || evidence.contains(".schema.json")
}

#[test]
fn every_native_limit_is_classified_once() {
    let document = read_json(&repo_path(CLASSIFICATION));
    let root = object(&document, CLASSIFICATION);
    assert_eq!(root["schema"], SCHEMA);
    assert_eq!(root["version"], VERSION);
    text(&root["counting"], "counting");

    let mut ids = BTreeSet::new();
    let mut classified_variants = BTreeSet::new();
    let mut classified_cases = BTreeMap::new();
    for (index, entry) in array(&root["limits"], "limits").iter().enumerate() {
        let fields = object(entry, &format!("limits[{index}]"));
        let keys: BTreeSet<&str> = fields.keys().map(String::as_str).collect();
        assert_eq!(keys, BTreeSet::from(ENTRY_KEYS), "limits[{index}] keys");
        let id = text(&fields["id"], "id");
        assert!(ids.insert(id.to_owned()), "{id}: repeated id");
        let crate_name = text(&fields["crate"], id);
        assert!(
            matches!(crate_name, "bake-session" | "bake-cli"),
            "{id}: crate"
        );
        let classification = text(&fields["classification"], id);
        assert!(
            CLASSIFICATIONS.contains(&classification),
            "{id}: classification {classification}"
        );
        let kind = text(&fields["kind"], id);
        assert!(KINDS.contains(&kind), "{id}: kind {kind}");
        let evidence = text(&fields["evidence"], id);
        assert!(!text(&fields["followUp"], id).is_empty(), "{id}: followUp");
        // A settled classification of a limit of its own needs a citation;
        // a pass-through row inherits the rows it names.
        if classification != "undecided" && kind != "pass-through" {
            assert!(
                cites_source(evidence),
                "{id}: evidence cites no source line"
            );
        }
        for variant in array(&fields["variants"], id) {
            let variant = text(variant, id);
            assert!(
                classified_variants.insert(variant.to_owned()),
                "{id}: {variant} is classified twice"
            );
        }
        let names = array(&fields["stageNames"], id);
        for name in names {
            assert!(!text(name, id).is_empty(), "{id}: empty stage name");
        }
        assert!(
            !array(&fields["variants"], id).is_empty() || !names.is_empty(),
            "{id}: names no limit"
        );
        for case in array(&fields["cases"], id) {
            let case = object(case, id);
            let keys: BTreeSet<&str> = case.keys().map(String::as_str).collect();
            assert_eq!(keys, BTreeSet::from(["count", "name", "table"]), "{id}");
            let table = text(&case["table"], id).to_owned();
            let name = text(&case["name"], id).to_owned();
            let count = case["count"]
                .as_u64()
                .unwrap_or_else(|| panic!("{id}: count"));
            assert!(count > 0, "{id}: zero count");
            assert!(
                classified_cases.insert((table, name), count).is_none(),
                "{id}: a table and limit name is classified twice"
            );
        }
    }

    let variants = source_variants();
    let unclassified: Vec<_> = variants.difference(&classified_variants).collect();
    let stale: Vec<_> = classified_variants.difference(&variants).collect();
    assert!(
        unclassified.is_empty(),
        "unclassified variants: {unclassified:?}"
    );
    assert!(
        stale.is_empty(),
        "classified variants not in src: {stale:?}"
    );

    let cases = conformance_limits();
    for ((table, name), count) in &cases {
        assert_eq!(
            classified_cases.get(&(table.clone(), name.clone())),
            Some(count),
            "{table} `{name}`: classified count differs from the table"
        );
    }
    for key in classified_cases.keys() {
        assert!(
            cases.contains_key(key),
            "{key:?}: classified but not in any table"
        );
    }
}

#[test]
fn variant_reader_sees_the_known_limit_enums() {
    // Guards the text reader itself: a formatting change that hid every
    // variant would otherwise pass the comparison above vacuously.
    let variants = source_variants();
    for known in [
        "ReplayLimit::Number",
        "RestoreLimit::Number",
        "ConsumedWorkCoercion::Reason",
        "EncodeLimit::FloatNumber",
        "V3Limit::SystemPayload",
    ] {
        assert!(variants.contains(known), "{known}");
    }
}
