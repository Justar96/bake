//! Runs every shared case in `conformance/session/plain-log-file-cases.json`
//! that applies to this host through `PlainLogFile`, each in a directory this
//! test owns and removes. A case seeds files and directories beneath its
//! root, then runs its steps in order: `create` and `open` start a handle,
//! `append` and `flush` use it, and `close` drops it. A step names its
//! handle, `a` unless `handle` says `b`. With no handle open, `move-root`
//! renames the root and `link-root` reaches it through a new symbolic link,
//! and later steps use that path. A case naming `compression` `zstd` runs
//! its create and open steps with `LogCompression::Zstd`, as the TypeScript
//! backend is configured with `compression: 'zstd'`, and a seed may give a
//! file's bytes as `hex`. After each step every file beneath
//! the root must have the hand-written text, or `<hex BYTES>` when its bytes
//! are not UTF-8, a `session.lock` file read only
//! by its size, since Windows refuses to read a locked range, and a symbolic
//! link its target, and no other file may exist. A thrown outcome maps to
//! the refusal Rust claims: the exact already-owned, already-exists,
//! not-found, duplicate-id, flat-layout, encoding-mismatch, stored-identity,
//! contiguity, lossless-snapshot, corruption, or unsupported-migration
//! message, with `{src}` rendered as the root joined with the step's `src`
//! path, `{srcJson}` as that path spelled by `JSON.stringify`, and `{dst}`
//! as the root joined with its `dst` path, or `Unadmitted` for any other
//! throw of a create or append. A `rust`
//! override names a native limit, or marks the step outside the model's
//! domain, which Rust does not run; either ends the case. Every limit must
//! be named by some case of the table, and every case that applies here must
//! witness the limit it names. Table strings, file names, and file text are
//! compared in `parse_json`'s spelling, so a U+FDD0 on disk is doubled.
//! Nothing here reads TypeScript output.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use bake_session::{
    AppendLimit, AppendRefusal, CreateLimit, CreateRefusal, LogCompression, LogFileLimit,
    LogFileRefusal, PlainLogFile,
    js_string::{from_rust, quote, to_rust},
};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/plain-log-file-cases";
const ORACLE: &str = "in an owned temporary root holding the seeded entries, run each step through the JSONL backend with the case's compression, none unless it names zstd, on the step's handle, a or b, each its own backend instance over the root: create, a write open, or the open handle's append, flush, or close, or, with no handle open, the root renamed or reached through a symbolic link to it; after each step list every file beneath the root with its text, or <hex BYTES> when its bytes are not UTF-8, an empty session.lock by its size, and every symbolic link by its target";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 109;
/// The Zstd cases' plaintext bound, far above any case's log.
const MAX_PLAINTEXT_BYTES: usize = 1 << 20;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 7] = [
    "empty-id",
    "encode",
    "seq-value",
    "windows-name",
    "non-utf8-name",
    "newer-generation",
    "scan",
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
    // Ids and expected messages may hold a lone surrogate, which only
    // `parse_json` reads.
    let table: Value = bake_session::parse_json(
        &std::fs::read_to_string(repo_path("conformance/session/plain-log-file-cases.json"))
            .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], 12);
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
            "darwin" => cfg!(target_os = "macos"),
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

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The case's `compression`: `zstd`, or none when absent.
fn compression(entry: &Map<String, Value>, id: &str) -> LogCompression {
    match entry.get("compression") {
        None => LogCompression::None,
        Some(value) => {
            assert_eq!(text(value, id), "zstd", "{id}: compression");
            LogCompression::Zstd {
                max_plaintext_bytes: MAX_PLAINTEXT_BYTES,
            }
        }
    }
}

fn seed(root: &Path, entry: &Value, id: &str) {
    let entry = object(entry, id);
    if let Some(file) = entry.get("file") {
        let path = root.join(&*to_rust(text(file, id)));
        std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed directory");
        if let Some(bytes) = entry.get("hex") {
            assert_eq!(keys(entry), BTreeSet::from(["file", "hex"]), "{id}: seed");
            std::fs::write(&path, hex(text(bytes, id))).expect("seed file");
        } else {
            assert_eq!(keys(entry), BTreeSet::from(["file", "text"]), "{id}: seed");
            std::fs::write(&path, &*to_rust(text(&entry["text"], id))).expect("seed file");
        }
        return;
    }
    if let Some(link) = entry.get("link") {
        assert_eq!(
            keys(entry),
            BTreeSet::from(["link", "target"]),
            "{id}: seed"
        );
        let target = text(&entry["target"], id);
        let path = root.join(text(link, id));
        symlink(Path::new(target), &path).expect("seed link");
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

/// Every file beneath `root`, by `/`-joined relative path, its text or
/// `<hex BYTES>` when it is not UTF-8; a `session.lock`
/// file is listed as empty when its size is 0 and otherwise by its size, and
/// a symbolic link as `<link to TARGET>`, not followed.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, prefix: &str, files: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("list") {
            let entry = entry.expect("entry");
            let name = from_rust(&entry.file_name().to_string_lossy()).into_owned();
            let relative = format!("{prefix}{name}");
            let kind = entry.file_type().expect("file type");
            if kind.is_symlink() {
                let target = std::fs::read_link(entry.path()).expect("link target");
                let target = from_rust(&target.to_string_lossy()).into_owned();
                files.insert(relative, format!("<link to {target}>"));
            } else if kind.is_dir() {
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
                let bytes = std::fs::read(entry.path()).expect("read a file");
                let text = match String::from_utf8(bytes) {
                    Ok(text) => from_rust(&text).into_owned(),
                    Err(bytes) => format!("<hex {}>", to_hex(bytes.as_bytes())),
                };
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
            | ("non-utf8-name", NativeSubset(LogFileLimit::NonUtf8Name))
            | (
                "newer-generation",
                NativeSubset(LogFileLimit::NewerGeneration)
            )
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
        ("create" | "open", "Error")
            if message.starts_with("session artifact ")
                && message.contains(" but this backend is configured for compression ") =>
        {
            "encoding-mismatch"
        }
        ("create" | "open", "Error")
            if message.starts_with("session artifact ")
                && message.contains(" uses the unsupported flat-file layout; ") =>
        {
            "legacy-layout"
        }
        ("open", "Error") if message.starts_with("corrupt session log ") => "stored-identity",
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
        LogFileRefusal::EncodingMismatch { .. } => "encoding-mismatch",
        LogFileRefusal::LegacyLayout { .. } => "legacy-layout",
        LogFileRefusal::StoredIdentity { .. } => "stored-identity",
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

/// The step's `key` path, `src` or `dst`, joined to `root`, which the
/// message's `placeholders` render; a message holds one of them exactly
/// when the step names `key`.
fn rendered_path(
    root: &Path,
    step: &Map<String, Value>,
    expected: Option<(&str, Option<&str>)>,
    (key, placeholders): (&str, &[&str]),
    context: &str,
) -> Option<String> {
    let placeholder = expected
        .and_then(|(_, message)| message)
        .is_some_and(|message| placeholders.iter().any(|held| message.contains(held)));
    assert_eq!(placeholder, step.contains_key(key), "{context}: {key}");
    let relative = text(step.get(key)?, context);
    let path = relative
        .split('/')
        .fold(root.to_path_buf(), |path, segment| {
            path.join(&*to_rust(segment))
        });
    Some(from_rust(&path.display().to_string()).into_owned())
}

/// `message` with its `{src}`, `{srcJson}`, and `{dst}` placeholders
/// rendered, all in `parse_json`'s spelling, as refusal messages are.
fn render(message: &str, source: &str, target: Option<&str>) -> String {
    let spelled = quote(source);
    let message = message
        .replace("{srcJson}", &spelled)
        .replace("{src}", source);
    target.map_or_else(
        || message.clone(),
        |target| message.replace("{dst}", target),
    )
}

/// A symbolic link at `path` to `target`; the cases that need one run on
/// POSIX only, since Windows needs a privilege to create one.
#[cfg(unix)]
fn symlink(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

/// No host but a Unix one creates the links these cases need.
#[cfg(not(unix))]
fn symlink(target: &Path, path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(format!(
        "linking {path:?} to {target:?} needs a Unix host"
    )))
}

/// `move-root` or `link-root`, run with every handle closed: the root's
/// new path, a sibling the step names, now holding or linking to it.
fn moved_root(root: &Path, name: &str, context: &str) -> PathBuf {
    let base = root.file_name().expect("root name").to_string_lossy();
    match name {
        "move-root" => {
            let next = root.with_file_name(format!("{base}-moved"));
            std::fs::rename(root, &next).expect("move the root");
            next
        }
        "link-root" => {
            let next = root.with_file_name(format!("{base}-link"));
            symlink(root, &next).expect("link the root");
            next
        }
        other => panic!("{context}: {other} is not a root step"),
    }
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
    compression: LogCompression,
    context: &str,
) -> Result<(), LogFileRefusal> {
    let name = text(&step["step"], context);
    let allowed: &[&str] = match name {
        "create" => &[
            "dst",
            "handle",
            "header",
            "inheritedEventCount",
            "rust",
            "src",
            "step",
            "tree",
            "ts",
        ],
        "open" => &["dst", "handle", "id", "rust", "src", "step", "tree", "ts"],
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
                PlainLogFile::create_compressed(root, &step["header"], count, compression)
            } else {
                PlainLogFile::open_compressed(
                    root,
                    text(&step["id"], context),
                    SOURCE_BUDGET,
                    compression,
                )
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
                "compression",
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
        let compression = compression(entry, id);
        let scratch = Scratch::new();
        // The root is a child, so `move-root` and `link-root` can name
        // siblings the scratch directory removes.
        let mut root = scratch.0.join("root");
        std::fs::create_dir(&root).expect("create the root");
        for seeded in entry["seed"].as_array().expect("seed") {
            seed(&root, seeded, id);
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
            if matches!(name, "move-root" | "link-root") {
                assert_eq!(
                    keys(step),
                    BTreeSet::from(["step", "tree", "ts"]),
                    "{context}: keys"
                );
                assert!(expected.is_none(), "{context}: {name} succeeds");
                assert!(
                    handles.values().all(Option::is_none),
                    "{context}: {name} needs every handle closed"
                );
                root = moved_root(&root, name, &context);
                assert_eq!(
                    tree(&root),
                    expected_tree(step, &context),
                    "{context}: tree"
                );
                continue;
            }
            let source = rendered_path(
                &root,
                step,
                expected,
                ("src", &["{src}", "{srcJson}"]),
                &context,
            );
            let target = rendered_path(&root, step, expected, ("dst", &["{dst}"]), &context);
            if let Some(rust) = step.get("rust")
                && override_limit(rust, &context).is_none()
            {
                break;
            }
            let actual = run_step(&root, step, &mut handles, compression, &context);
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
                            |source| render(message, source, target.as_deref()),
                        );
                        assert_eq!(refusal.message(), Some(message.as_str()), "{context}");
                    }
                    refusals.insert(expected_kind);
                }
                (expected, actual) => panic!("{context}: expected {expected:?}, got {actual:?}"),
            }
            assert_eq!(
                tree(&root),
                expected_tree(step, &context),
                "{context}: tree"
            );
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
            "encoding-mismatch",
            "legacy-layout",
            "not-found",
            "not-lossless",
            "seq-mismatch",
            "stored-identity",
            "unsupported"
        ]),
        "every claimed refusal is witnessed"
    );
}

/// A write `open` of a Zstd log refuses to decode more plaintext than its
/// bound, a native limit TypeScript does not have, and writes nothing; the
/// whole plaintext is within an exact bound.
#[test]
fn a_zstd_open_refuses_past_its_plaintext_budget() {
    let scratch = Scratch::new();
    let header = serde_json::json!({
        "version": 3, "id": "b1", "createdAt": 1, "isSeeded": false, "delegationDepth": 0
    });
    let mut file =
        PlainLogFile::create_compressed(&scratch.0, &header, None, zstd(1)).expect("create");
    let event = serde_json::json!({"type": "turn/start", "seq": 0, "time": 1, "data": {"turn": 1}});
    file.append(&[event]).expect("append");
    let plaintext = file.log().bytes().expect("written").len();
    let stored = file.stored_bytes().expect("written").to_vec();
    drop(file);
    let before = tree(&scratch.0);
    let refused =
        PlainLogFile::open_compressed(&scratch.0, "b1", SOURCE_BUDGET, zstd(plaintext - 1))
            .expect_err("past the bound");
    assert!(
        matches!(refused, LogFileRefusal::NativePlaintextBudget { max_plaintext_bytes } if max_plaintext_bytes == plaintext - 1),
        "{refused:?}"
    );
    assert_eq!(refused.message(), None);
    assert_eq!(tree(&scratch.0), before);
    let opened = PlainLogFile::open_compressed(&scratch.0, "b1", SOURCE_BUDGET, zstd(plaintext))
        .expect("an exact bound");
    assert_eq!(opened.stored_bytes(), Some(stored.as_slice()));
    assert_eq!(opened.compression(), zstd(plaintext));
    drop(opened);

    // An older Zstd generation is decoded within the same budget and refused
    // the same way before anything is written.
    let scratch = Scratch::new();
    let id = "zstd-migrate-v2-seeded";
    let case = load()
        .into_iter()
        .find(|entry| entry["id"] == id)
        .expect("the seeded Zstd v2 case");
    for entry in case["seed"].as_array().expect("seed") {
        seed(&scratch.0, entry, id);
    }
    let before = tree(&scratch.0);
    let refused = PlainLogFile::open_compressed(&scratch.0, "v2v3", SOURCE_BUDGET, zstd(1))
        .expect_err("past the bound");
    assert!(
        matches!(
            refused,
            LogFileRefusal::NativePlaintextBudget {
                max_plaintext_bytes: 1
            }
        ),
        "{refused:?}"
    );
    assert_eq!(refused.message(), None);
    let mut after = tree(&scratch.0);
    // The lock is taken before the source is read, as on the current path.
    after.remove("_no-cwd/v2v3/session.lock");
    assert_eq!(after, before);
    PlainLogFile::open_compressed(&scratch.0, "v2v3", SOURCE_BUDGET, zstd(MAX_PLAINTEXT_BYTES))
        .expect("within the bound");
}

const fn zstd(max_plaintext_bytes: usize) -> LogCompression {
    LogCompression::Zstd {
        max_plaintext_bytes,
    }
}
