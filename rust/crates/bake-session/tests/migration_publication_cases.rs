//! Runs every shared case in `conformance/session/migration-publication-cases.json`
//! that applies to this host through `PlainLogFile`, each in a directory
//! this test owns and removes. A case seeds files, then write-opens a v2
//! Session through the hidden `storage_io` seam: `RaceIo` passes every
//! operation to the real filesystem, except that when the writer creates
//! its `session.migration.*` temporary file it first applies the case's
//! race actions, as another process would, and injects the case's fault at
//! that creation, at the temporary file's first write, or at the hard link
//! that publishes it (D30). The remaining steps append to and drop the
//! handle. After each step every file beneath the root must have the
//! hand-written text, a `session.lock` file read only by its size, and a
//! symbolic link its target, and no other file may exist.
//!
//! A thrown outcome maps to the refusal Rust claims: the exact corruption or
//! changed-source message, with `{src}` and `{dst}` rendered as the root
//! joined with the step's `src` and `dst` paths, or an I/O failure whose
//! kind is the errno's. A `rust` override names the native limit Rust
//! refuses the step by; the refused open must leave the tree the step
//! states, and it ends the case. Nothing here reads TypeScript output.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bake_session::storage_io::{FileStat, ListedEntry, LockFailure, RealIo, StorageIo};
use bake_session::{LogFileLimit, LogFileRefusal, PlainLogFile};
use serde_json::{Map, Value};

const SCHEMA: &str = "bake/session-conformance/migration-publication-cases";
const ORACLE: &str = "in an owned temporary root holding the seeded entries, write-open the Session through the JSONL backend with compression none; when the backend creates its session.migration temporary file, first apply the case's race actions, writing, replacing through a rename, or removing a file, or creating a symbolic link, beneath the root, and inject the case's fault, an errno at that creation, at the temporary file's first write, or at the hard link; then run the remaining steps on the handle, and after each step list every file beneath the root with its text, an empty session.lock by its size, and every symbolic link by its target";
const VERSION: u64 = 1;
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 15;
const SOURCE_BUDGET: usize = 64;
const LIMITS: [&str; 1] = ["migration/target-tail"];
const FAULT_OPS: [&str; 3] = ["create-temporary", "write-temporary", "link"];
const CLASSES: [&str; 2] = [
    "SessionPersistenceCorruptionError",
    "JsonlGenerationSourceChangedError",
];
const LEASE_FILE: &str = "session.lock";
const TEMPORARY_PREFIX: &str = "session.migration.";

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

/// `relative`, `/`-separated, beneath `root`.
fn beneath(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

/// The errno a case names, as the kind a Rust I/O failure reports.
fn errno_kind(code: &str) -> ErrorKind {
    match code {
        "ENOENT" => ErrorKind::NotFound,
        "EACCES" | "EPERM" => ErrorKind::PermissionDenied,
        "ENOSPC" => ErrorKind::StorageFull,
        other => panic!("unknown errno {other}"),
    }
}

// ---------------------------------------------------------------------------
// The racing implementation.

/// What another process does, and which operation fails, when the writer
/// creates its migration temporary file.
#[derive(Debug)]
struct Race {
    root: PathBuf,
    actions: Vec<Value>,
    fault: Option<(String, ErrorKind)>,
    /// Apply the actions when the writer opens `session.lock`, after it
    /// first selected a generation and before it holds the lock, instead
    /// of at the temporary file's creation.
    at_lock: bool,
}

#[derive(Debug, Default)]
struct RaceState {
    fired: bool,
    /// The temporary file awaits its first write.
    unwritten: bool,
}

#[derive(Debug)]
struct RaceIo {
    race: Race,
    state: Mutex<RaceState>,
}

fn is_temporary(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(TEMPORARY_PREFIX))
}

fn injected(kind: ErrorKind) -> io::Error {
    io::Error::new(kind, "injected fault")
}

impl RaceIo {
    fn state(&self) -> std::sync::MutexGuard<'_, RaceState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fault(&self, op: &str) -> Option<ErrorKind> {
        self.race
            .fault
            .as_ref()
            .filter(|(name, _)| name == op)
            .map(|(_, kind)| *kind)
    }

    fn fired(&self) -> bool {
        self.state().fired
    }

    /// Apply the race actions beneath the root.
    fn apply(&self) {
        for action in &self.race.actions {
            let action = object(action, "race action");
            if let Some(path) = action.get("write") {
                let path = beneath(&self.race.root, text(path, "write"));
                std::fs::create_dir_all(path.parent().expect("parent")).expect("race directory");
                std::fs::write(&path, text(&action["text"], "text")).expect("race write");
            } else if let Some(path) = action.get("replace") {
                let path = beneath(&self.race.root, text(path, "replace"));
                let name = path.file_name().expect("name").to_string_lossy();
                let sibling = path.with_file_name(format!(".replacement-{name}"));
                std::fs::write(&sibling, text(&action["text"], "text")).expect("race write");
                std::fs::rename(&sibling, &path).expect("race replace");
            } else if let Some(path) = action.get("remove") {
                std::fs::remove_file(beneath(&self.race.root, text(path, "remove")))
                    .expect("race remove");
            } else {
                let target = text(&action["target"], "target");
                let path = beneath(&self.race.root, text(&action["link"], "link"));
                symlink(Path::new(target), &path).expect("race link");
            }
        }
    }
}

impl StorageIo for RaceIo {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        RealIo.create_dir_all(dir)
    }
    fn create_new(&self, path: &Path) -> io::Result<File> {
        if is_temporary(path) && !self.race.at_lock {
            let first = !std::mem::replace(&mut self.state().fired, true);
            if first {
                self.apply();
                if let Some(kind) = self.fault("create-temporary") {
                    return Err(injected(kind));
                }
                let file = RealIo.create_new(path)?;
                self.state().unwritten = true;
                return Ok(file);
            }
        }
        RealIo.create_new(path)
    }
    fn open_write(&self, path: &Path) -> io::Result<File> {
        RealIo.open_write(path)
    }
    fn write_at(&self, file: &mut File, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if std::mem::take(&mut self.state().unwritten)
            && let Some(kind) = self.fault("write-temporary")
        {
            return Err(injected(kind));
        }
        RealIo.write_at(file, offset, bytes)
    }
    fn set_len(&self, file: &File, len: u64) -> io::Result<()> {
        RealIo.set_len(file, len)
    }
    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        if is_temporary(original)
            && let Some(kind) = self.fault("link")
        {
            return Err(injected(kind));
        }
        RealIo.hard_link(original, link)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        RealIo.remove_file(path)
    }
    fn rename_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        RealIo.rename_new(from, to)
    }
    fn sync_file(&self, file: &File, path: &Path) -> io::Result<()> {
        RealIo.sync_file(file, path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        RealIo.sync_dir(dir)
    }
    fn open_lock(&self, path: &Path) -> io::Result<File> {
        if self.race.at_lock && !std::mem::replace(&mut self.state().fired, true) {
            self.apply();
        }
        RealIo.open_lock(path)
    }
    fn try_lock(&self, file: &File) -> Result<(), LockFailure> {
        RealIo.try_lock(file)
    }
    fn lock_is_current(&self, held: &File, path: &Path) -> io::Result<bool> {
        RealIo.lock_is_current(held, path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        RealIo.read(path)
    }
    fn read_dir(&self, dir: &Path) -> io::Result<Option<Vec<ListedEntry>>> {
        RealIo.read_dir(dir)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        RealIo.canonicalize(path)
    }
    fn probe(&self, path: &Path) -> io::Result<bool> {
        RealIo.probe(path)
    }
    fn stat_dir(&self, path: &Path) -> io::Result<Option<bool>> {
        RealIo.stat_dir(path)
    }
    fn stat(&self, path: &Path, follow: bool) -> io::Result<FileStat> {
        RealIo.stat(path, follow)
    }
}

/// A symbolic link at `path` to `target`; the cases that need one run on
/// POSIX only, since Windows needs a privilege to create one.
#[cfg(unix)]
fn symlink(target: &Path, path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn symlink(target: &Path, path: &Path) -> io::Result<()> {
    Err(io::Error::other(format!(
        "linking {path:?} to {target:?} needs a Unix host"
    )))
}

// ---------------------------------------------------------------------------
// The table.

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bake-session-migration-publication-{}-{count}",
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
    let table: Value = bake_session::parse_json(
        &std::fs::read_to_string(repo_path(
            "conformance/session/migration-publication-cases.json",
        ))
        .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "oracle", "schema", "version"])
    );
    assert_eq!(table["schema"], SCHEMA);
    assert_eq!(table["version"], VERSION);
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

/// Every file beneath `root`, by `/`-joined relative path; a `session.lock`
/// file is listed as empty when its size is 0 and otherwise by its size, and
/// a symbolic link as `<link to TARGET>`, not followed.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, prefix: &str, files: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("list") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = format!("{prefix}{name}");
            let kind = entry.file_type().expect("file type");
            if kind.is_symlink() {
                let target = std::fs::read_link(entry.path()).expect("link target");
                files.insert(relative, format!("<link to {}>", target.to_string_lossy()));
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

/// The step's expected outcome.
#[derive(Debug)]
enum Expected {
    Ok,
    Thrown { class: String, message: String },
    Errno(String),
}

fn expected(step: &Map<String, Value>, context: &str) -> Expected {
    let ts = object(&step["ts"], context);
    match text(&ts["outcome"], context) {
        "ok" => {
            assert_eq!(keys(ts), BTreeSet::from(["outcome"]), "{context}");
            Expected::Ok
        }
        "thrown" => {
            assert_eq!(
                keys(ts),
                BTreeSet::from(["class", "message", "outcome"]),
                "{context}"
            );
            let class = text(&ts["class"], context);
            assert!(CLASSES.contains(&class), "{context}: class {class}");
            Expected::Thrown {
                class: class.to_owned(),
                message: text(&ts["message"], context).to_owned(),
            }
        }
        "errno" => {
            assert_eq!(keys(ts), BTreeSet::from(["code", "outcome"]), "{context}");
            Expected::Errno(text(&ts["code"], context).to_owned())
        }
        other => panic!("{context}: unknown outcome {other}"),
    }
}

/// The root rendered into a message, in `parse_json`'s spelling.
fn spelled(path: &Path) -> String {
    bake_session::js_string::from_rust(&path.display().to_string()).into_owned()
}

/// Check `actual` against the step's expected outcome; returns the class or
/// kind witnessed.
fn check(
    root: &Path,
    step: &Map<String, Value>,
    actual: Result<(), LogFileRefusal>,
    context: &str,
) -> String {
    match (expected(step, context), actual) {
        (Expected::Ok, Ok(())) => "ok".to_owned(),
        (Expected::Thrown { class, message }, Err(refusal)) => {
            let claimed = match &refusal {
                LogFileRefusal::Corrupt { .. } => "SessionPersistenceCorruptionError",
                LogFileRefusal::SourceChanged { .. } => "JsonlGenerationSourceChangedError",
                other => panic!("{context}: expected {class}, got {other:?}"),
            };
            assert_eq!(claimed, class, "{context}");
            let mut message = message;
            for (key, placeholder) in [("src", "{src}"), ("dst", "{dst}")] {
                assert_eq!(
                    message.contains(placeholder),
                    step.contains_key(key),
                    "{context}: {key}"
                );
                if let Some(path) = step.get(key) {
                    let path = spelled(&beneath(root, text(path, context)));
                    message = message.replace(placeholder, &path);
                }
            }
            assert_eq!(refusal.message(), Some(message.as_str()), "{context}");
            class
        }
        (Expected::Errno(code), Err(LogFileRefusal::Io(error))) => {
            assert_eq!(error.kind(), errno_kind(&code), "{context}: {error}");
            code
        }
        (expected, actual) => panic!("{context}: expected {expected:?}, got {actual:?}"),
    }
}

fn limit_matches(name: &str, refusal: &LogFileRefusal) -> bool {
    name.strip_prefix("migration/").is_some_and(|limit| {
        matches!(refusal, LogFileRefusal::NativeSubset(LogFileLimit::Migration(name)) if name == limit)
    })
}

/// The `rust` override's limit, checked against the schema.
fn override_limit<'a>(step: &'a Map<String, Value>, context: &str) -> Option<&'a str> {
    let rust = object(step.get("rust")?, context);
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

fn race_of(entry: &Map<String, Value>, root: &Path, id: &str) -> Race {
    let actions = entry["race"].as_array().expect("race").clone();
    for action in &actions {
        let action = object(action, id);
        let shape = keys(action);
        assert!(
            [
                BTreeSet::from(["text", "write"]),
                BTreeSet::from(["replace", "text"]),
                BTreeSet::from(["remove"]),
                BTreeSet::from(["link", "target"]),
            ]
            .contains(&shape),
            "{id}: race action {shape:?}"
        );
    }
    let fault = entry.get("fault").map(|fault| {
        let fault = object(fault, id);
        assert_eq!(keys(fault), BTreeSet::from(["code", "op"]), "{id}: fault");
        let op = text(&fault["op"], id);
        assert!(FAULT_OPS.contains(&op), "{id}: fault op {op}");
        (op.to_owned(), errno_kind(text(&fault["code"], id)))
    });
    Race {
        root: root.to_path_buf(),
        actions,
        fault,
        at_lock: false,
    }
}

#[test]
fn shared_cases_publish_migrations_like_the_typescript_backend() {
    let cases = load();
    assert_eq!(cases.len(), CASE_COUNT);
    let ids: BTreeSet<&str> = cases
        .iter()
        .map(|entry| text(&entry["id"], "case id"))
        .collect();
    assert_eq!(ids.len(), CASE_COUNT);
    let mut witnessed = BTreeSet::new();
    let mut outcomes = BTreeSet::new();
    let mut ran = 0;
    for entry in &cases {
        let id = text(&entry["id"], "case id");
        assert!(
            keys(entry).is_subset(&BTreeSet::from([
                "id",
                "platforms",
                "platformReason",
                "seed",
                "race",
                "fault",
                "steps",
                "note"
            ])),
            "{id}: unknown keys"
        );
        if !applies(entry, id) {
            continue;
        }
        ran += 1;
        let scratch = Scratch::new();
        let root = scratch.0.join("root");
        std::fs::create_dir(&root).expect("create the root");
        for seed in entry["seed"].as_array().expect("seed") {
            let seed = object(seed, id);
            assert_eq!(keys(seed), BTreeSet::from(["file", "text"]), "{id}: seed");
            let path = beneath(&root, text(&seed["file"], id));
            std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed directory");
            std::fs::write(&path, text(&seed["text"], id)).expect("seed file");
        }
        let io = Arc::new(RaceIo {
            race: race_of(entry, &root, id),
            state: Mutex::default(),
        });
        // Declared after the scratch directory, so the handle, and the lock
        // Windows would not let the directory be removed under, drop first.
        let mut handle: Option<PlainLogFile> = None;
        for (index, step) in entry["steps"].as_array().expect("steps").iter().enumerate() {
            let context = format!("{id} step {index}");
            let step = object(step, &context);
            let name = text(&step["step"], &context);
            assert_eq!(index == 0, name == "open", "{context}: open comes first");
            let actual = match name {
                "open" => {
                    let opened = PlainLogFile::open_with_io(
                        io.clone(),
                        &root,
                        text(&step["id"], &context),
                        SOURCE_BUDGET,
                    );
                    assert!(
                        io.fired(),
                        "{context}: the migration created its temporary file"
                    );
                    opened.map(|opened| handle = Some(opened))
                }
                "append" => handle
                    .as_mut()
                    .expect("an open handle")
                    .append(step["events"].as_array().expect("events")),
                "close" => {
                    assert!(handle.take().is_some(), "{context}: close needs a handle");
                    Ok(())
                }
                other => panic!("{context}: unknown step {other}"),
            };
            if let Some(limit) = override_limit(step, &context) {
                let refusal = actual.expect_err(&format!("{context}: a limit refuses"));
                assert!(limit_matches(limit, &refusal), "{context}: {refusal:?}");
                witnessed.insert(limit.to_owned());
                assert_eq!(
                    tree(&root),
                    expected_tree(step, &context),
                    "{context}: tree"
                );
                break;
            }
            outcomes.insert(check(&root, step, actual, &context));
            assert_eq!(
                tree(&root),
                expected_tree(step, &context),
                "{context}: tree"
            );
        }
    }
    assert!(ran > 0);
    assert_eq!(
        witnessed,
        LIMITS.iter().map(|name| (*name).to_owned()).collect(),
        "every limit is witnessed"
    );
    let mut claimed: BTreeSet<String> = ["ok", "ENOENT", "EACCES", "ENOSPC"]
        .iter()
        .chain(CLASSES.iter())
        .map(|name| (*name).to_owned())
        .collect();
    if cfg!(not(windows)) {
        claimed.insert("EPERM".to_owned());
    }
    assert_eq!(outcomes, claimed, "every claimed outcome is witnessed");
}

/// `requireStoredLog` selects the generation again once the lock is held, so
/// a v3 log another lock-holding writer published, and appended to, between
/// the first selection and the lock is opened, not migrated over. This
/// reuses the target text of the shared `target-with-tail-published-first`
/// case, whose race would otherwise reach `migration/target-tail`.
#[test]
fn a_generation_published_before_the_lock_is_selected_again() {
    let cases = load();
    let entry = cases
        .iter()
        .find(|entry| entry["id"] == "target-with-tail-published-first")
        .expect("the tail case");
    let scratch = Scratch::new();
    let root = scratch.0.join("root");
    std::fs::create_dir(&root).expect("create the root");
    for seed in entry["seed"].as_array().expect("seed") {
        let path = beneath(&root, text(&seed["file"], "seed"));
        std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed directory");
        std::fs::write(&path, text(&seed["text"], "seed")).expect("seed file");
    }
    let mut race = race_of(entry, &root, "reselect");
    race.at_lock = true;
    let io = Arc::new(RaceIo {
        race,
        state: Mutex::default(),
    });
    let before = tree(&root);
    let opened = PlainLogFile::open_with_io(io.clone(), &root, "v2v3", SOURCE_BUDGET);
    assert!(io.fired(), "the race ran when the lock was opened");
    let opened = opened.expect("the published generation opens");
    let target = beneath(&root, "_no-cwd/v2v3/session.v3.jsonl");
    assert_eq!(opened.path(), target);
    let mut after = before;
    after.insert(
        "_no-cwd/v2v3/session.v3.jsonl".to_owned(),
        text(&entry["race"][0]["text"], "race text").to_owned(),
    );
    assert_eq!(tree(&root), after, "no migration was written");
    drop(opened);
}
