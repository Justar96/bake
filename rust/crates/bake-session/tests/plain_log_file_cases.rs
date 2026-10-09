//! Runs every shared case in `conformance/session/plain-log-file-cases.json`
//! that applies to this host through `PlainLogFile`, each in a directory this
//! test owns and removes. A case seeds files and directories beneath its
//! root, then runs its steps in order: `create` and `open` start a handle,
//! `append` and `flush` use it, and `close` drops it. A step names its
//! handle, `a` unless `handle` says `b`. After each step every file beneath
//! the root must have the hand-written text, a `session.lock` file read only
//! by its size, since Windows refuses to read a locked range, and no other
//! file may exist. A thrown outcome maps to the refusal Rust claims: the
//! exact already-owned, already-exists, not-found, duplicate-id, contiguity,
//! lossless-snapshot, corruption, or unsupported-migration message, with `{src}` rendered as the root joined with the step's `src`
//! path, or `Unadmitted` for any other throw of a create or append. A `rust`
//! override names a native limit, or marks the step outside the model's
//! domain, which Rust does not run; either ends the case. Every limit must
//! be named by some case of the table, and every case that applies here must
//! witness the limit it names. Nothing here reads TypeScript output.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use bake_session::{
    AppendLimit, AppendRefusal, CreateLimit, CreateRefusal, LogFileLimit, LogFileRefusal,
    PlainLogFile,
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/plain-log-file-cases";
const ORACLE: &str = "in an owned temporary root holding the seeded entries, run each step through the JSONL backend with compression none on the step's handle, a or b, each its own backend instance over the root: create, a write open, or the open handle's append, flush, or close; after each step list every file beneath the root with its text, an empty session.lock by its size";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 64;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 12] = [
    "empty-id",
    "encode",
    "seq-value",
    "windows-name",
    "legacy-layout",
    "opposite-encoding",
    "non-utf8-name",
    "newer-generation",
    "identity",
    "scan",
    "migration/v2-codec-recovery",
    "migration/row/json-parser",
];
const CLASSES: [&str; 8] = [
    "Error",
    "TypeError",
    "SessionFormatError",
    "SessionAlreadyExistsError",
    "SessionAlreadyOwnedError",
    "SessionPersistenceNotFoundError",
    "SessionPersistenceCorruptionError",
    "SessionFormatUnsupportedError",
];
const NOT_LOSSLESS: &str = "session event batch is not losslessly JSON-serializable because it contains non-JSON-serializable data";
const LEASE_FILE: &str = "session.lock";
const HANDLES: [&str; 2] = ["a", "b"];

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

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bake-session-plain-log-file-{}-{count}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create an unused scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn load() -> Vec<Map<String, Value>> {
    let table: Value = serde_json::from_slice(
        &std::fs::read(repo_path("conformance/session/plain-log-file-cases.json"))
            .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 5);
    assert_eq!(table["oracle"], ORACLE);
    assert!(
        table["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|entry| object(entry, "case").clone())
        .collect()
}

/// Whether the case's `platforms` include this host.
fn applies(entry: &Map<String, Value>, id: &str) -> bool {
    assert_eq!(
        entry.contains_key("platforms"),
        entry.contains_key("platformReason"),
        "{id}: platforms need a reason"
    );
    let Some(platforms) = entry.get("platforms") else {
        return true;
    };
    text(&entry["platformReason"], id);
    platforms
        .as_array()
        .expect("platforms")
        .iter()
        .any(|platform| match text(platform, id) {
            "posix" => cfg!(not(windows)),
            "linux" => cfg!(target_os = "linux"),
            "win32" => cfg!(windows),
            other => panic!("{id}: unknown platform {other}"),
        })
}

fn hex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2) && text.is_ascii(),
        "hex {text}"
    );
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn seed(root: &Path, entry: &Value, id: &str) {
    let entry = object(entry, id);
    if let Some(file) = entry.get("file") {
        assert_eq!(keys(entry), BTreeSet::from(["file", "text"]), "{id}: seed");
        let path = root.join(text(file, id));
        std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed directory");
        std::fs::write(&path, text(&entry["text"], id)).expect("seed file");
        return;
    }
    assert_eq!(
        keys(entry),
        BTreeSet::from(["dir", "rawNameHex"]),
        "{id}: seed"
    );
    let parent = root.join(text(&entry["dir"], id));
    std::fs::create_dir_all(&parent).expect("seed directory");
    let raw = hex(text(&entry["rawNameHex"], id));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::create_dir(parent.join(std::ffi::OsStr::from_bytes(&raw))).expect("raw name");
    }
    #[cfg(not(unix))]
    panic!("{id}: raw names {raw:?} need a Unix host");
}

/// Every file beneath `root`, by `/`-joined relative path; a `session.lock`
/// file is listed as empty when its size is 0 and otherwise by its size.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, prefix: &str, files: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("list") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = format!("{prefix}{name}");
            if entry.file_type().expect("file type").is_dir() {
                walk(&entry.path(), &format!("{relative}/"), files);
            } else if name == LEASE_FILE {
                let size = entry.metadata().expect("lock metadata").len();
                let text = if size == 0 {
                    String::new()
                } else {
                    format!("<{size} bytes>")
                };
                files.insert(relative, text);
            } else {
                let text = std::fs::read_to_string(entry.path()).expect("UTF-8 file");
                files.insert(relative, text);
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, "", &mut files);
    files
}

fn expected_tree(step: &Map<String, Value>, context: &str) -> BTreeMap<String, String> {
    object(&step["tree"], context)
        .iter()
        .map(|(path, text_value)| (path.clone(), text(text_value, context).to_owned()))
        .collect()
}

/// A `rust` override's limit name, or `None` for a step outside the
/// model's domain, checked against the schema.
fn override_limit<'a>(rust: &'a Value, context: &str) -> Option<&'a str> {
    let rust = object(rust, context);
    if rust["outcome"] == "outside-domain" {
        assert_eq!(keys(rust), BTreeSet::from(["outcome"]), "{context}");
        return None;
    }
    assert_eq!(
        keys(rust),
        BTreeSet::from(["limit", "outcome"]),
        "{context}"
    );
    assert_eq!(rust["outcome"], "native-subset", "{context}");
    let name = text(&rust["limit"], context);
    assert!(LIMITS.contains(&name), "{context}: unknown limit {name}");
    Some(name)
}

fn limit_matches(name: &str, refusal: &LogFileRefusal) -> bool {
    use LogFileRefusal::{Append, Create, NativeSubset};
    if let Some(migration) = name.strip_prefix("migration/") {
        return matches!(refusal, NativeSubset(LogFileLimit::Migration(limit)) if limit == migration);
    }
    matches!(
        (name, refusal),
        (
            "empty-id",
            Create(CreateRefusal::NativeSubset(CreateLimit::EmptyId))
                | NativeSubset(LogFileLimit::EmptyId)
        ) | (
            "encode",
            Create(CreateRefusal::NativeSubset(CreateLimit::Encode(_)))
                | Append(AppendRefusal::NativeSubset(AppendLimit::Encode(_)))
        ) | (
            "seq-value",
            Append(AppendRefusal::NativeSubset(AppendLimit::SeqValue))
        ) | ("windows-name", NativeSubset(LogFileLimit::WindowsName))
            | ("legacy-layout", NativeSubset(LogFileLimit::LegacyLayout))
            | (
                "opposite-encoding",
                NativeSubset(LogFileLimit::OppositeEncoding)
            )
            | ("non-utf8-name", NativeSubset(LogFileLimit::NonUtf8Name))
            | (
                "newer-generation",
                NativeSubset(LogFileLimit::NewerGeneration)
            )
            | ("identity", NativeSubset(LogFileLimit::Identity))
            | ("scan", NativeSubset(LogFileLimit::Scan(_)))
    )
}

/// The kind of refusal Rust claims for a TypeScript throw of `step`.
fn expected_kind(step: &str, class: &str, message: &str) -> &'static str {
    match (step, class) {
        (_, "SessionAlreadyOwnedError") => "already-owned",
        (_, "SessionAlreadyExistsError") => "already-exists",
        (_, "SessionPersistenceNotFoundError") => "not-found",
        (_, "Error") if message.starts_with("duplicate JSONL session id ") => "duplicate",
        ("append", "Error") if message.starts_with("append seq mismatch for ") => "seq-mismatch",
        ("append", "TypeError") if message == NOT_LOSSLESS => "not-lossless",
        ("open", "SessionPersistenceCorruptionError") => "corrupt",
        ("open", "SessionFormatUnsupportedError") => "unsupported",
        ("append", _) => "append-unadmitted",
        ("create", _) => "create-unadmitted",
        _ => panic!("{step}: a {class} throw needs a rust limit"),
    }
}

fn kind(refusal: &LogFileRefusal) -> &'static str {
    match refusal {
        LogFileRefusal::AlreadyOwned { .. } => "already-owned",
        LogFileRefusal::AlreadyExists { .. } => "already-exists",
        LogFileRefusal::NotFound { .. } => "not-found",
        LogFileRefusal::Duplicate { .. } => "duplicate",
        LogFileRefusal::Corrupt { .. } => "corrupt",
        LogFileRefusal::Unsupported { .. } => "unsupported",
        LogFileRefusal::Append(AppendRefusal::SeqMismatch { .. }) => "seq-mismatch",
        LogFileRefusal::Append(AppendRefusal::NotLossless) => "not-lossless",
        LogFileRefusal::Append(AppendRefusal::Unadmitted) => "append-unadmitted",
        LogFileRefusal::Create(CreateRefusal::Unadmitted) => "create-unadmitted",
        _ => "other",
    }
}

/// The step's `ts` outcome: `None` for success, or the thrown class and message.
fn outcome<'a>(step: &'a Map<String, Value>, context: &str) -> Option<(&'a str, Option<&'a str>)> {
    let ts = object(&step["ts"], context);
    match text(&ts["outcome"], context) {
        "ok" => {
            assert_eq!(keys(ts), BTreeSet::from(["outcome"]), "{context}");
            None
        }
        "thrown" => {
            let class = text(&ts["class"], context);
            assert!(CLASSES.contains(&class), "{context}: class {class}");
            let message = ts.get("message").map(|message| text(message, context));
            let expected_keys = if message.is_some() { 3 } else { 2 };
            assert_eq!(keys(ts).len(), expected_keys, "{context}: thrown keys");
            Some((class, message))
        }
        other => panic!("{context}: unknown outcome {other}"),
    }
}

/// The step's `src` path joined to `root`, which a `{src}` placeholder in
/// its message renders; a message holds the placeholder exactly when the
/// step names `src`.
fn rendered_source(
    root: &Path,
    step: &Map<String, Value>,
    expected: Option<(&str, Option<&str>)>,
    context: &str,
) -> Option<String> {
    let placeholder = expected
        .and_then(|(_, message)| message)
        .is_some_and(|message| message.contains("{src}"));
    assert_eq!(placeholder, step.contains_key("src"), "{context}: src");
    let source = text(step.get("src")?, context);
    let path = source
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment));
    Some(path.display().to_string())
}

/// Every limit a `rust` override of the case names.
fn named_limits(entry: &Map<String, Value>, id: &str) -> Vec<String> {
    entry["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .filter_map(|step| step.get("rust"))
        .filter_map(|rust| override_limit(rust, id).map(str::to_owned))
        .collect()
}

/// Run one step on its handle, which a create or open fills.
fn run_step(
    root: &Path,
    step: &Map<String, Value>,
    handles: &mut BTreeMap<&'static str, Option<PlainLogFile>>,
    context: &str,
) -> Result<(), LogFileRefusal> {
    let name = text(&step["step"], context);
    let allowed: &[&str] = match name {
        "create" => &[
            "handle",
            "header",
            "inheritedEventCount",
            "rust",
            "step",
            "tree",
            "ts",
        ],
        "open" => &["handle", "id", "rust", "src", "step", "tree", "ts"],
        "append" => &["events", "handle", "rust", "step", "tree", "ts"],
        "flush" => &["handle", "rust", "step", "tree", "ts"],
        "close" => &["handle", "step", "tree", "ts"],
        other => panic!("{context}: unknown step {other}"),
    };
    assert!(
        keys(step).is_subset(&allowed.iter().copied().collect()),
        "{context}: keys"
    );
    let label = step.get("handle").map_or("a", |label| text(label, context));
    let label = *HANDLES
        .iter()
        .find(|known| **known == label)
        .unwrap_or_else(|| panic!("{context}: unknown handle {label}"));
    let handle = handles.entry(label).or_default();
    match name {
        "create" | "open" => {
            assert!(handle.is_none(), "{context}: one open value per handle");
            let opened = if name == "create" {
                let count = step
                    .get("inheritedEventCount")
                    .map(|count| count.as_u64().expect("count"));
                PlainLogFile::create(root, &step["header"], count)
            } else {
                PlainLogFile::open(root, text(&step["id"], context), SOURCE_BUDGET)
            };
            *handle = Some(opened?);
            Ok(())
        }
        "close" => {
            assert!(handle.take().is_some(), "{context}: close needs a handle");
            assert!(
                outcome(step, context).is_none(),
                "{context}: close succeeds"
            );
            Ok(())
        }
        _ => {
            let log = handle
                .as_mut()
                .unwrap_or_else(|| panic!("{context}: {name} needs a handle"));
            if name == "flush" {
                log.flush()
            } else {
                log.append(step["events"].as_array().expect("events"))
            }
        }
    }
}

#[test]
fn shared_cases_lay_out_and_write_like_the_typescript_backend() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases
        .iter()
        .map(|entry| text(&entry["id"], "case id"))
        .collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let named: BTreeSet<String> = cases
        .iter()
        .flat_map(|entry| named_limits(entry, text(&entry["id"], "case id")))
        .collect();
    assert_eq!(
        named,
        LIMITS.iter().map(|name| (*name).to_owned()).collect(),
        "the table names every limit"
    );
    let mut witnessed = BTreeSet::new();
    let mut refusals = BTreeSet::new();
    let mut ran = 0;
    for entry in &cases {
        let id = text(&entry["id"], "case id");
        assert!(
            keys(entry).is_subset(&BTreeSet::from([
                "id",
                "platforms",
                "platformReason",
                "seed",
                "steps",
                "note"
            ])),
            "{id}: unknown keys"
        );
        assert!(
            entry.get("note").is_none_or(Value::is_string),
            "{id}: invalid note"
        );
        if !applies(entry, id) {
            continue;
        }
        ran += 1;
        let scratch = Scratch::new();
        let root = scratch.0.as_path();
        for seeded in entry["seed"].as_array().expect("seed") {
            seed(root, seeded, id);
        }
        // Declared after the scratch directory, so the handles, and the
        // locks Windows would not let the directory be removed under, drop
        // first, on a panic too.
        let mut handles = BTreeMap::new();
        for (index, step) in entry["steps"].as_array().expect("steps").iter().enumerate() {
            let context = format!("{id} step {index}");
            let step = object(step, &context);
            let name = text(&step["step"], &context);
            let expected = outcome(step, &context);
            let source = rendered_source(root, step, expected, &context);
            if let Some(rust) = step.get("rust")
                && override_limit(rust, &context).is_none()
            {
                break;
            }
            let actual = run_step(root, step, &mut handles, &context);
            if let Some(rust) = step.get("rust") {
                let limit = override_limit(rust, &context).expect("a limit");
                let refusal = actual.expect_err(&format!("{context}: a limit refuses"));
                assert!(limit_matches(limit, &refusal), "{context}: {refusal:?}");
                witnessed.insert(limit.to_owned());
                break;
            }
            match (expected, actual) {
                (None, Ok(())) => {}
                (Some((class, message)), Err(refusal)) => {
                    let message =
                        message.unwrap_or_else(|| panic!("{context}: a throw needs a message"));
                    let expected_kind = expected_kind(name, class, message);
                    assert_eq!(kind(&refusal), expected_kind, "{context}: {refusal:?}");
                    if !expected_kind.ends_with("unadmitted") {
                        let message = source.as_deref().map_or_else(
                            || message.to_owned(),
                            |source| message.replace("{src}", source),
                        );
                        assert_eq!(refusal.message(), Some(message.as_str()), "{context}");
                    }
                    refusals.insert(expected_kind);
                }
                (expected, actual) => panic!("{context}: expected {expected:?}, got {actual:?}"),
            }
            assert_eq!(tree(root), expected_tree(step, &context), "{context}: tree");
        }
    }
    assert!(ran > 0);
    let applicable: BTreeSet<String> = cases
        .iter()
        .filter(|entry| applies(entry, text(&entry["id"], "case id")))
        .flat_map(|entry| named_limits(entry, text(&entry["id"], "case id")))
        .collect();
    assert_eq!(witnessed, applicable, "every applicable limit is witnessed");
    assert_eq!(
        refusals,
        BTreeSet::from([
            "already-exists",
            "already-owned",
            "append-unadmitted",
            "corrupt",
            "duplicate",
            "not-found",
            "not-lossless",
            "seq-mismatch",
            "unsupported"
        ]),
        "every claimed refusal is witnessed"
    );
}
