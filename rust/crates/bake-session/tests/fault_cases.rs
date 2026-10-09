//! Deterministic fault injection (D30) for the development Session writer,
//! `PlainLogFile`, through its `storage_io` seam, against the shared post-crash
//! states in `conformance/session/fault-cases.json`.
//!
//! `FaultIo` wraps the real filesystem and counts every `StorageIo` operation.
//! It can fail operation `i` with a disk-full, permission, or I/O error, tear
//! a write at `i` so only the first half of its bytes reach the file, or crash
//! at `i`: operation `i` does not run (or, torn, runs half of its write) and
//! every later operation fails, so no cleanup reaches the disk. A crashed
//! handle is then dropped, which closes its files and releases its lock as
//! the kernel does when a process dies; the writer has no drop-time writes.
//!
//! Each write scenario runs once fault-free to count its operations, then, in
//! a fresh root each time, with a crash at every operation index and a torn
//! crash at every write. After each run every older generation the scenario
//! seeded must be byte-identical, and the root's files, a temporary file's
//! random token normalized, must be the seed of a case in the table whose
//! `scenarios` list it. That case's steps, whose outcomes and trees were
//! written from the TypeScript backend's sources and which the TypeScript
//! spec `fault-conformance.spec.ts` asserts against the backend over the same
//! seeded files, then run through the real filesystem on the crashed root
//! itself: the reopen takes the write lock, so a lock a crashed holder kept
//! fails it. Every case must be reached by its scenario, so the table holds
//! exactly the reachable states. The error-path sweep fails each operation of
//! each scenario once with each error kind, and tears each write at half with
//! each error kind: the operation must report the error, the sources must be
//! unchanged, and the root must hold a table state that some crash without a
//! torn write also leaves, so a failed write is rolled back. A failed removal
//! of a temporary file already linked into place is swallowed instead.
//!
//! The `#[ignore]`d full sweep also tears every write at every byte count:
//! it checks the sources and that only the lock, canonical generations, and
//! temporary files remain, and reopens the root with the steps of the state
//! torn at half of that write. A torn temporary file is never read, so its
//! reopen must reach the same outcomes and final log; a torn log append must
//! too when the cut keeps as many whole rows and does not end a line, and
//! otherwise only the open's outcome is compared.
//!
//! ```text
//! cargo test --locked -p bake-session --test fault_cases -- --ignored --exact full_torn_write_sweep
//! ```
//!
//! With `BAKE_FAULT_DUMP` naming a file, each state no case seeds is also
//! appended to it as a JSON line, the input a new table version starts from.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bake_session::storage_io::{ListedEntry, LockFailure, RealIo, StorageIo, StorageOp};
use bake_session::{
    LogFileRefusal, PlainLogFile,
    js_string::{from_rust, to_rust},
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/fault-cases";
const ORACLE: &str = "in an owned temporary root holding the seeded files, a state a crashed or failed Session writer leaves, run each step through the JSONL backend with compression none on one handle: a write open, create, or the open handle's append, flush, or close; after each step list every file beneath the root with its text and an empty session.lock by its size";
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 42;
const SOURCE_BUDGET: usize = 64;
const LEASE_FILE: &str = "session.lock";
/// The token a temporary file's name is normalized to.
const TOKEN: &str = "000000000000";
const CLASSES: [&str; 2] = [
    "SessionPersistenceNotFoundError",
    "SessionPersistenceCorruptionError",
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

// ---------------------------------------------------------------------------
// The faulting implementation.

/// An error a fault reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    StorageFull,
    PermissionDenied,
    Eio,
}

impl Failure {
    const ALL: [Self; 3] = [Self::StorageFull, Self::PermissionDenied, Self::Eio];

    fn error(self) -> io::Error {
        match self {
            Self::StorageFull => io::Error::new(ErrorKind::StorageFull, "injected ENOSPC"),
            Self::PermissionDenied => {
                io::Error::new(ErrorKind::PermissionDenied, "injected EACCES")
            }
            Self::Eio => io::Error::other("injected EIO"),
        }
    }
}

/// What happens at the chosen operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The operation does not run, and it and every later one fail.
    Crash,
    /// A write stores its first `keep` bytes, then it and every later
    /// operation fail.
    TornCrash { keep: usize },
    /// The operation does not run and reports the error; later ones run.
    Fail(Failure),
    /// A write stores its first `keep` bytes and reports the error; later
    /// operations run.
    TornFail { keep: usize, failure: Failure },
}

#[derive(Debug, Default)]
struct FaultState {
    ops: Vec<StorageOp>,
    /// The bytes of each write, by operation index.
    writes: BTreeMap<usize, Vec<u8>>,
    crashed: bool,
}

/// The real filesystem with one deterministic fault at operation `at`.
#[derive(Debug)]
struct FaultIo {
    fault: Option<(usize, Fault)>,
    state: Mutex<FaultState>,
}

/// What an operation does under the plan.
enum Gate {
    Run,
    Torn { keep: usize, error: io::Error },
    Fail(io::Error),
}

fn crashed() -> io::Error {
    io::Error::other("injected crash: the process is gone")
}

impl FaultIo {
    fn new(fault: Option<(usize, Fault)>) -> Arc<Self> {
        Arc::new(Self {
            fault,
            state: Mutex::new(FaultState::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FaultState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ops(&self) -> Vec<StorageOp> {
        self.state().ops.clone()
    }

    fn writes(&self) -> BTreeMap<usize, Vec<u8>> {
        self.state().writes.clone()
    }

    fn gate(&self, op: StorageOp, written: Option<&[u8]>) -> Gate {
        let mut state = self.state();
        let index = state.ops.len();
        state.ops.push(op);
        if let Some(bytes) = written {
            state.writes.insert(index, bytes.to_vec());
        }
        if state.crashed {
            return Gate::Fail(crashed());
        }
        let Some((at, fault)) = self.fault else {
            return Gate::Run;
        };
        if at != index {
            return Gate::Run;
        }
        match fault {
            Fault::Crash => {
                state.crashed = true;
                Gate::Fail(crashed())
            }
            Fault::TornCrash { keep } => {
                state.crashed = true;
                if written.is_some() {
                    Gate::Torn {
                        keep,
                        error: crashed(),
                    }
                } else {
                    Gate::Fail(crashed())
                }
            }
            Fault::Fail(failure) => Gate::Fail(failure.error()),
            Fault::TornFail { keep, failure } => {
                if written.is_some() {
                    Gate::Torn {
                        keep,
                        error: failure.error(),
                    }
                } else {
                    Gate::Fail(failure.error())
                }
            }
        }
    }

    fn run<T>(&self, op: StorageOp, real: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        match self.gate(op, None) {
            Gate::Run => real(),
            Gate::Fail(error) | Gate::Torn { error, .. } => Err(error),
        }
    }
}

impl StorageIo for FaultIo {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.run(StorageOp::CreateDir, || RealIo.create_dir_all(dir))
    }

    fn create_new(&self, path: &Path) -> io::Result<File> {
        self.run(StorageOp::CreateNew, || RealIo.create_new(path))
    }

    fn open_write(&self, path: &Path) -> io::Result<File> {
        self.run(StorageOp::OpenWrite, || RealIo.open_write(path))
    }

    fn write_at(&self, file: &mut File, offset: u64, bytes: &[u8]) -> io::Result<()> {
        match self.gate(StorageOp::WriteAt, Some(bytes)) {
            Gate::Run => RealIo.write_at(file, offset, bytes),
            Gate::Fail(error) => Err(error),
            Gate::Torn { keep, error } => {
                RealIo.write_at(file, offset, &bytes[..keep.min(bytes.len())])?;
                Err(error)
            }
        }
    }

    fn set_len(&self, file: &File, len: u64) -> io::Result<()> {
        self.run(StorageOp::SetLen, || RealIo.set_len(file, len))
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.run(StorageOp::HardLink, || RealIo.hard_link(original, link))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.run(StorageOp::RemoveFile, || RealIo.remove_file(path))
    }

    fn open_lock(&self, path: &Path) -> io::Result<File> {
        self.run(StorageOp::OpenLock, || RealIo.open_lock(path))
    }

    fn try_lock(&self, file: &File) -> Result<(), LockFailure> {
        match self.gate(StorageOp::TryLock, None) {
            Gate::Run => RealIo.try_lock(file),
            Gate::Fail(error) | Gate::Torn { error, .. } => Err(LockFailure::Io(error)),
        }
    }

    fn lock_is_current(&self, held: &File, path: &Path) -> io::Result<bool> {
        self.run(StorageOp::LockIsCurrent, || {
            RealIo.lock_is_current(held, path)
        })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.run(StorageOp::Read, || RealIo.read(path))
    }

    fn read_dir(&self, dir: &Path) -> io::Result<Option<Vec<ListedEntry>>> {
        self.run(StorageOp::ReadDir, || RealIo.read_dir(dir))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        self.run(StorageOp::Canonicalize, || RealIo.canonicalize(path))
    }

    fn probe(&self, path: &Path) -> io::Result<bool> {
        self.run(StorageOp::Probe, || RealIo.probe(path))
    }
}

// ---------------------------------------------------------------------------
// Scenarios.

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bake-session-fault-{}-{count}", std::process::id()));
        std::fs::create_dir(&path).expect("create an unused scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One writer action of a scenario, on its single handle.
#[derive(Debug, Clone)]
enum Action {
    Create(Value),
    Open(&'static str),
    Append(Vec<Value>),
    Flush,
}

/// A write scenario: the files it seeds and the actions it runs.
#[derive(Debug)]
struct Scenario {
    name: &'static str,
    seed: Vec<(&'static str, String)>,
    actions: Vec<Action>,
}

fn header(id: &str) -> Value {
    json!({"version": 3, "id": id, "createdAt": 1, "isSeeded": false, "delegationDepth": 0})
}

fn header_line(id: &str) -> String {
    format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"createdAt\":1,\"isSeeded\":false,\"delegationDepth\":0}}\n"
    )
}

fn turn_start(seq: u64, turn: u64) -> Value {
    json!({"type": "turn/start", "seq": seq, "time": seq + 1, "data": {"turn": turn}})
}

fn turn_end(seq: u64, turn: u64) -> Value {
    json!({"type": "turn/end", "seq": seq, "time": seq + 1, "data": {"turn": turn}})
}

fn line(event: &Value) -> String {
    format!("{}\n", serde_json::to_string(event).expect("event line"))
}

/// The released source generations the migration scenarios seed, from
/// `plain-log-file-cases.json`'s `migrate-v0-packed-run`,
/// `migrate-v1-packed-run`, and `migrate-v2-seeded` cases.
fn released_seed(case: &str) -> (String, String) {
    // The table holds lone surrogates elsewhere, which only `parse_json` reads.
    let table = bake_session::parse_json(
        &std::fs::read_to_string(repo_path("conformance/session/plain-log-file-cases.json"))
            .expect("read plain-log-file table"),
    )
    .expect("parse plain-log-file table");
    let found = table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|entry| entry["id"] == case)
        .unwrap_or_else(|| panic!("plain-log-file case {case}"));
    let seed = &found["seed"][0];
    (
        seed["file"].as_str().expect("file").to_owned(),
        seed["text"].as_str().expect("text").to_owned(),
    )
}

fn scenarios() -> Vec<Scenario> {
    let stored = format!("{}{}", header_line("w"), line(&turn_start(0, 1)));
    let torn = format!(
        "{stored}{{\"type\":\"ext/note\",\"seq\":1,\"time\":2,\"ignorable\":true,\"data\":{{\"text\":\"torn"
    );
    let mut list = vec![
        Scenario {
            name: "create-flush",
            seed: vec![],
            actions: vec![Action::Create(header("w")), Action::Flush],
        },
        Scenario {
            name: "create-append",
            seed: vec![],
            actions: vec![
                Action::Create(header("w")),
                Action::Append(vec![turn_start(0, 1), turn_end(1, 1)]),
            ],
        },
        Scenario {
            name: "create-flush-appends",
            seed: vec![],
            actions: vec![
                Action::Create(header("w")),
                Action::Flush,
                Action::Append(vec![turn_start(0, 1)]),
                Action::Flush,
                Action::Append(vec![turn_end(1, 1)]),
            ],
        },
        Scenario {
            name: "open-append",
            seed: vec![("_no-cwd/w/session.v3.jsonl", stored)],
            actions: vec![
                Action::Open("w"),
                Action::Append(vec![turn_end(1, 1), turn_start(2, 2)]),
            ],
        },
        Scenario {
            name: "open-torn-append",
            seed: vec![("_no-cwd/w/session.v3.jsonl", torn)],
            actions: vec![Action::Open("w"), Action::Append(vec![turn_end(1, 1)])],
        },
    ];
    for (name, case, id) in [
        ("migrate-v0", "migrate-v0-packed-run", "hist"),
        ("migrate-v1", "migrate-v1-packed-run", "hist"),
        ("migrate-v2", "migrate-v2-seeded", "v2v3"),
    ] {
        let (file, text) = released_seed(case);
        let file: &'static str = Box::leak(file.into_boxed_str());
        list.push(Scenario {
            name,
            seed: vec![(file, text)],
            actions: vec![Action::Open(id), Action::Flush],
        });
    }
    list
}

/// Run the scenario's actions through `io` until one fails, then drop the
/// handle as a dead process would. The first refusal is returned.
fn run_actions(io: &Arc<FaultIo>, root: &Path, scenario: &Scenario) -> Option<LogFileRefusal> {
    let shared: Arc<dyn StorageIo> = io.clone();
    let mut handle: Option<PlainLogFile> = None;
    for action in &scenario.actions {
        let outcome = match action {
            Action::Create(header) => {
                PlainLogFile::create_with_io(shared.clone(), root, header, None)
                    .map(|created| handle = Some(created))
            }
            Action::Open(id) => PlainLogFile::open_with_io(shared.clone(), root, id, SOURCE_BUDGET)
                .map(|opened| handle = Some(opened)),
            Action::Append(events) => handle.as_mut().expect("a handle").append(events),
            Action::Flush => handle.as_mut().expect("a handle").flush(),
        };
        if let Err(refusal) = outcome {
            return Some(refusal);
        }
    }
    None
}

fn seed_root(root: &Path, scenario: &Scenario) {
    for (file, text) in &scenario.seed {
        let path = root.join(&*to_rust(file));
        std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed directory");
        std::fs::write(&path, &*to_rust(text)).expect("seed file");
    }
}

/// Every seeded older generation must be byte-identical; the current
/// generation a scenario seeds is the log it appends to.
fn check_sources(root: &Path, scenario: &Scenario, context: &str) {
    for (file, text) in &scenario.seed {
        if file.ends_with("/session.v3.jsonl") {
            continue;
        }
        let bytes = std::fs::read(root.join(&*to_rust(file))).expect("source generation");
        assert!(
            bytes == to_rust(text).as_bytes(),
            "{context}: source {file} changed"
        );
    }
}

/// A temporary file's name with its random token normalized, or `None` for
/// any other name.
fn normalized_temporary(name: &str) -> Option<String> {
    let token_of = |prefix: &str| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .filter(|token| !token.is_empty() && !token.contains('/'))
            .map(|_| format!("{prefix}{TOKEN}.tmp"))
    };
    token_of("session.v3.jsonl.").or_else(|| token_of("session.migration."))
}

/// Every file beneath `root`, a `session.lock` by its size and a temporary
/// file under its normalized name.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, prefix: &str, files: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("list") {
            let entry = entry.expect("entry");
            let name = from_rust(&entry.file_name().to_string_lossy()).into_owned();
            let kind = entry.file_type().expect("file type");
            if kind.is_dir() {
                walk(&entry.path(), &format!("{prefix}{name}/"), files);
                continue;
            }
            let text = if name == LEASE_FILE {
                let size = entry.metadata().expect("lock metadata").len();
                if size == 0 {
                    String::new()
                } else {
                    format!("<{size} bytes>")
                }
            } else {
                let text = std::fs::read_to_string(entry.path()).expect("UTF-8 file");
                from_rust(&text).into_owned()
            };
            let name = normalized_temporary(&name).unwrap_or(name);
            assert!(
                files.insert(format!("{prefix}{name}"), text).is_none(),
                "two temporaries in one directory"
            );
        }
    }
    let mut files = BTreeMap::new();
    walk(root, "", &mut files);
    files
}

// ---------------------------------------------------------------------------
// The table.

struct Case {
    id: String,
    scenarios: Vec<String>,
    /// Each seeded file's text, a hard link's that of the file it links.
    seed: BTreeMap<String, String>,
    /// Seeded hard links and the seeded files they link.
    links: BTreeMap<String, String>,
    steps: Vec<Map<String, Value>>,
}

fn text<'a>(value: &'a Value, context: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a string"))
}

fn object<'a>(value: &'a Value, context: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected an object"))
}

fn keys(fields: &Map<String, Value>) -> BTreeSet<&str> {
    fields.keys().map(String::as_str).collect()
}

fn string_map(value: &Value, context: &str) -> BTreeMap<String, String> {
    object(value, context)
        .iter()
        .map(|(path, text_value)| (path.clone(), text(text_value, context).to_owned()))
        .collect()
}

fn load() -> Vec<Case> {
    let table = bake_session::parse_json(
        &std::fs::read_to_string(repo_path("conformance/session/fault-cases.json"))
            .expect("read table"),
    )
    .expect("parse table");
    let table = object(&table, "table");
    assert_eq!(
        keys(table),
        BTreeSet::from(["cases", "history", "oracle", "schema", "version"])
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
    let cases: Vec<Case> = table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|entry| {
            let entry = object(entry, "case");
            let id = text(&entry["id"], "case id").to_owned();
            let allowed = BTreeSet::from(["id", "note", "scenarios", "seed", "steps"]);
            assert!(keys(entry).is_subset(&allowed), "{id}: keys");
            let scenarios = entry["scenarios"]
                .as_array()
                .expect("scenarios")
                .iter()
                .map(|name| text(name, &id).to_owned())
                .collect();
            let mut seed = BTreeMap::new();
            let mut links = BTreeMap::new();
            for file in entry["seed"].as_array().expect("seed") {
                let file = object(file, &id);
                let path = text(&file["file"], &id).to_owned();
                if let Some(target) = file.get("hardLinkTo") {
                    assert_eq!(keys(file), BTreeSet::from(["file", "hardLinkTo"]), "{id}");
                    links.insert(path, text(target, &id).to_owned());
                } else {
                    assert_eq!(keys(file), BTreeSet::from(["file", "text"]), "{id}: seed");
                    seed.insert(path, text(&file["text"], &id).to_owned());
                }
            }
            for (link, target) in &links {
                let target_text = seed.get(target).expect("a seeded link target").clone();
                seed.insert(link.clone(), target_text);
            }
            let steps = entry["steps"]
                .as_array()
                .expect("steps")
                .iter()
                .map(|step| object(step, &id).clone())
                .collect();
            Case {
                id,
                scenarios,
                seed,
                links,
                steps,
            }
        })
        .collect();
    assert_eq!(cases.len(), CASE_COUNT, "case count");
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len(), "duplicate case id");
    cases
}

/// The refusal class a TypeScript throw of this table maps to.
fn refusal_class(refusal: &LogFileRefusal) -> Option<&'static str> {
    match refusal {
        LogFileRefusal::NotFound { .. } => Some("SessionPersistenceNotFoundError"),
        LogFileRefusal::Corrupt { .. } => Some("SessionPersistenceCorruptionError"),
        _ => None,
    }
}

/// How much of a case [`run_case`] compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strictness {
    /// Every step's outcome and tree.
    Exact,
    /// Every step's outcome, and the last step's tree without temporary
    /// files: the root holds a variant of the case's seed whose torn bytes
    /// differ.
    FinalLog,
    /// The first step's outcome alone.
    OpenOnly,
}

/// `tree` without temporary files.
fn without_temporaries(tree: BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.into_iter()
        .filter(|(name, _)| !name.ends_with(&format!(".{TOKEN}.tmp")))
        .collect()
}

/// Run `case`'s steps through the real filesystem over `root`, which holds
/// the case's seed, or a variant `strictness` allows.
fn run_case(case: &Case, root: &Path, strictness: Strictness) {
    let mut handle: Option<PlainLogFile> = None;
    for (index, step) in case.steps.iter().enumerate() {
        let context = format!("{} step {index}", case.id);
        let name = text(&step["step"], &context);
        let outcome = match name {
            "open" => PlainLogFile::open(root, text(&step["id"], &context), SOURCE_BUDGET)
                .map(|opened| handle = Some(opened)),
            "create" => PlainLogFile::create(root, &step["header"], None)
                .map(|created| handle = Some(created)),
            "append" => handle
                .as_mut()
                .expect("a handle")
                .append(step["events"].as_array().expect("events")),
            "flush" => handle.as_mut().expect("a handle").flush(),
            "close" => {
                handle = None;
                Ok(())
            }
            other => panic!("{context}: unknown step {other}"),
        };
        if strictness == Strictness::OpenOnly && index > 0 {
            return;
        }
        let ts = object(&step["ts"], &context);
        match (text(&ts["outcome"], &context), outcome) {
            ("ok", Ok(())) => {}
            ("thrown", Err(refusal)) => {
                let class = text(&ts["class"], &context);
                assert!(CLASSES.contains(&class), "{context}: class {class}");
                assert_eq!(
                    refusal_class(&refusal),
                    Some(class),
                    "{context}: {refusal:?}"
                );
                let message = text(&ts["message"], &context);
                assert_eq!(refusal.message(), Some(message), "{context}");
            }
            (expected, outcome) => panic!("{context}: expected {expected}, got {outcome:?}"),
        }
        match strictness {
            Strictness::Exact => {
                assert_eq!(tree(root), string_map(&step["tree"], &context), "{context}");
            }
            Strictness::FinalLog if index + 1 == case.steps.len() => assert_eq!(
                without_temporaries(tree(root)),
                without_temporaries(string_map(&step["tree"], &context)),
                "{context}"
            ),
            Strictness::FinalLog | Strictness::OpenOnly => {}
        }
    }
}

/// The table case whose seed is `state` among those naming `scenario`.
fn state_case<'a>(
    cases: &'a [Case],
    scenario: &str,
    state: &BTreeMap<String, String>,
) -> Option<&'a Case> {
    cases
        .iter()
        .find(|case| case.scenarios.iter().any(|name| name == scenario) && case.seed == *state)
}

fn dump_unknown(scenario: &str, context: &str, state: &BTreeMap<String, String>) -> String {
    format!(
        "{context}: scenario {scenario} reached a state no case seeds: {}",
        serde_json::to_string(state).expect("state")
    )
}

/// One run of `scenario` with `fault` in a fresh root: the sources must be
/// unchanged and the state a case of the scenario, whose steps then run over
/// that root. Returns the first refusal and the state.
fn faulted_run(
    cases: &[Case],
    scenario: &Scenario,
    fault: Option<(usize, Fault)>,
    reached: &mut BTreeSet<String>,
    unknown: &mut Vec<String>,
) -> (Option<LogFileRefusal>, BTreeMap<String, String>) {
    let scratch = Scratch::new();
    seed_root(&scratch.0, scenario);
    let io = FaultIo::new(fault);
    let refusal = run_actions(&io, &scratch.0, scenario);
    let context = format!("{} {fault:?}", scenario.name);
    check_sources(&scratch.0, scenario, &context);
    let state = tree(&scratch.0);
    match state_case(cases, scenario.name, &state) {
        Some(case) => {
            reached.insert(case.id.clone());
            run_case(case, &scratch.0, Strictness::Exact);
        }
        None => {
            if let Ok(path) = std::env::var("BAKE_FAULT_DUMP") {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)
                    .expect("dump");
                writeln!(
                    file,
                    "{}",
                    json!({"scenario": scenario.name, "fault": format!("{fault:?}"), "state": state})
                )
                .expect("dump");
            }
            unknown.push(dump_unknown(scenario.name, &context, &state));
        }
    }
    (refusal, state)
}

/// The fault-free run's operations and the bytes of its writes.
fn count(scenario: &Scenario) -> (Vec<StorageOp>, BTreeMap<usize, Vec<u8>>) {
    let scratch = Scratch::new();
    seed_root(&scratch.0, scenario);
    let io = FaultIo::new(None);
    let refusal = run_actions(&io, &scratch.0, scenario);
    assert!(refusal.is_none(), "{}: {refusal:?}", scenario.name);
    (io.ops(), io.writes())
}

/// Whether operation `index` removes a temporary file already linked into
/// place.
fn published_temporary_removal(ops: &[StorageOp], index: usize) -> bool {
    ops[index] == StorageOp::RemoveFile && index > 0 && ops[index - 1] == StorageOp::HardLink
}

#[test]
fn every_crash_point_leaves_a_state_typescript_reopens_alike() {
    let cases = load();
    let mut reached = BTreeSet::new();
    let mut unknown = Vec::new();
    for scenario in scenarios() {
        let (ops, writes) = count(&scenario);
        assert!(
            ops.contains(&StorageOp::TryLock),
            "{}: no lock taken",
            scenario.name
        );
        for index in 0..=ops.len() {
            let (refusal, _) = faulted_run(
                &cases,
                &scenario,
                Some((index, Fault::Crash)),
                &mut reached,
                &mut unknown,
            );
            // A crash at the removal of a published temporary is swallowed,
            // as TypeScript swallows a failed removal there, and only a
            // later operation reports it.
            if index == ops.len() || !published_temporary_removal(&ops, index) {
                assert_eq!(
                    refusal.is_some(),
                    index < ops.len(),
                    "{} {index}",
                    scenario.name
                );
            }
            if let Some(bytes) = writes.get(&index) {
                faulted_run(
                    &cases,
                    &scenario,
                    Some((
                        index,
                        Fault::TornCrash {
                            keep: bytes.len() / 2,
                        },
                    )),
                    &mut reached,
                    &mut unknown,
                );
            }
        }
    }
    assert!(unknown.is_empty(), "{}", unknown.join("\n"));
    let all: BTreeSet<String> = cases.iter().map(|case| case.id.clone()).collect();
    let unreached: Vec<&String> = all.difference(&reached).collect();
    assert!(
        unreached.is_empty(),
        "cases no crash or failure reaches: {unreached:?}"
    );
}

/// A failed operation reports its error and leaves a state a crash at some
/// operation, without a torn write, also leaves: a failed publication removes
/// its temporary file, and a failed append write is rolled back, as
/// TypeScript's `rollbackAppend` does, so no torn bytes of the batch remain.
#[test]
fn every_failed_operation_reports_and_leaves_a_table_state() {
    let cases = load();
    let mut reached = BTreeSet::new();
    let mut unknown = Vec::new();
    for scenario in scenarios() {
        let (ops, writes) = count(&scenario);
        let clean: Vec<BTreeMap<String, String>> = (0..=ops.len())
            .map(|index| {
                faulted_run(
                    &cases,
                    &scenario,
                    Some((index, Fault::Crash)),
                    &mut reached,
                    &mut unknown,
                )
                .1
            })
            .collect();
        for index in 0..ops.len() {
            let mut faults: Vec<Fault> = Failure::ALL.into_iter().map(Fault::Fail).collect();
            if let Some(bytes) = writes.get(&index) {
                for failure in Failure::ALL {
                    faults.push(Fault::TornFail {
                        keep: bytes.len() / 2,
                        failure,
                    });
                }
            }
            for fault in faults {
                let (refusal, state) = faulted_run(
                    &cases,
                    &scenario,
                    Some((index, fault)),
                    &mut reached,
                    &mut unknown,
                );
                // A failed removal of a published temporary is swallowed, as
                // TypeScript's is, and the run goes on with that second link.
                if published_temporary_removal(&ops, index) {
                    assert!(refusal.is_none(), "{} {index}: {refusal:?}", scenario.name);
                    continue;
                }
                assert!(
                    clean.contains(&state),
                    "{} {index} {fault:?}: a state no untorn crash leaves: {state:?}",
                    scenario.name
                );
                assert!(
                    matches!(refusal, Some(LogFileRefusal::Io(_))),
                    "{} {index} {fault:?}: {refusal:?}",
                    scenario.name
                );
            }
        }
    }
    assert!(unknown.is_empty(), "{}", unknown.join("\n"));
}

/// Every write torn at every byte count, crashing; see the module comment.
#[test]
#[ignore = "the full torn-write sweep; the default sweeps tear each write at half"]
fn full_torn_write_sweep() {
    let cases = load();
    for scenario in scenarios() {
        let (ops, writes) = count(&scenario);
        for (&index, bytes) in &writes {
            let half = run_torn(&scenario, index, bytes.len() / 2);
            let reference = state_case(&cases, scenario.name, &half)
                .unwrap_or_else(|| panic!("{} {index}: the half-torn state", scenario.name));
            for keep in 0..bytes.len() {
                let scratch = Scratch::new();
                seed_root(&scratch.0, &scenario);
                let io = FaultIo::new(Some((index, Fault::TornCrash { keep })));
                run_actions(&io, &scratch.0, &scenario);
                let context = format!("{} {index} keep {keep} of {:?}", scenario.name, ops[index]);
                check_sources(&scratch.0, &scenario, &context);
                for name in tree(&scratch.0).keys() {
                    let base = name.rsplit('/').next().unwrap_or_default();
                    assert!(
                        base == LEASE_FILE
                            || base.ends_with(&format!(".{TOKEN}.tmp"))
                            || canonical_generation_name(base),
                        "{context}: stray {name}"
                    );
                }
                // A torn temporary is never read, so any cut reopens as the
                // half cut does. A torn log append reopens alike only when
                // the cut leaves as many whole rows as the half cut, and not
                // at a line end, which TypeScript's scan reads as no torn tail.
                let publication = index > 0 && ops[index - 1] == StorageOp::CreateNew;
                let rows = |cut: usize| bytes[..cut].iter().filter(|&&byte| byte == b'\n').count();
                let ends_line = keep > 0 && bytes[keep - 1] == b'\n';
                let strictness =
                    if publication || (!ends_line && rows(keep) == rows(bytes.len() / 2)) {
                        Strictness::FinalLog
                    } else {
                        Strictness::OpenOnly
                    };
                run_case(reference, &scratch.0, strictness);
            }
        }
    }
}

/// The state `scenario` leaves when write `index` is torn at `keep` bytes.
fn run_torn(scenario: &Scenario, index: usize, keep: usize) -> BTreeMap<String, String> {
    let scratch = Scratch::new();
    seed_root(&scratch.0, scenario);
    run_actions(
        &FaultIo::new(Some((index, Fault::TornCrash { keep }))),
        &scratch.0,
        scenario,
    );
    tree(&scratch.0)
}

/// Whether `name` is a canonical plain generation's name.
fn canonical_generation_name(name: &str) -> bool {
    matches!(
        name,
        "session.jsonl" | "session.v1.jsonl" | "session.v2.jsonl" | "session.v3.jsonl"
    )
}

#[test]
fn shared_cases_reopen_like_the_typescript_backend() {
    for case in load() {
        let scratch = Scratch::new();
        for (file, file_text) in &case.seed {
            if case.links.contains_key(file) {
                continue;
            }
            let path = scratch.0.join(&*to_rust(file));
            std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed dir");
            std::fs::write(&path, &*to_rust(file_text)).expect("seed file");
        }
        for (link, target) in &case.links {
            std::fs::hard_link(
                scratch.0.join(&*to_rust(target)),
                scratch.0.join(&*to_rust(link)),
            )
            .expect("seed link");
        }
        run_case(&case, &scratch.0, Strictness::Exact);
    }
}
