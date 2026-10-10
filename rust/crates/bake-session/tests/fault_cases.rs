//! Deterministic fault injection (D30) for the development Session writer,
//! `PlainLogFile`, through its `storage_io` seam, against the shared post-crash
//! states in `conformance/session/fault-cases.json`.
//!
//! `FaultIo` wraps the real filesystem and counts every `StorageIo` operation.
//! It runs either write sequence on any host: POSIX, with directory syncs and
//! hard-link publication, or Win32, with write-through moves and no directory
//! sync, as the seam's write platform selects. It can fail operation `i` with
//! a disk-full, permission, or I/O error, tear a write at `i` so only the
//! first half of its bytes reach the file, crash at `i`, so operation `i`
//! does not run (or, torn, runs half of its write) and every later operation
//! fails and no cleanup reaches the disk, or cut the power at `i`. A crashed
//! handle is then dropped, which closes its files and releases its lock as
//! the kernel does when a process dies; the writer has no drop-time writes.
//!
//! A power cut keeps only what a sync made durable. `FaultIo` mirrors the
//! namespace beneath the root, files and directories as nodes so a hard link
//! shares its file's node, and records what each sync makes durable: a file
//! sync its file's bytes, a POSIX directory sync the directory's entries,
//! and a Win32 write-through move the moved entry. The seeded files are
//! durable. After a power cut the root is rebuilt from durable entries
//! alone: an entry whose directory's entries were never synced is gone, and
//! a file holds the bytes its last sync saw, none if it was never synced.
//! `FaultIo` records syncs in that model and does not sync the real files.
//!
//! Each write scenario runs once fault-free on each platform to count its
//! operations, then, in a fresh root each time, with a crash, a power cut,
//! and, at a write, a crash tearing it at half, at every operation index.
//! After each run every older generation the scenario seeded must be
//! byte-identical, and the root's files, a temporary file's random token
//! normalized in creation order, and its hard links must be the seed of a
//! case in the table whose `scenarios` list it. That case's steps, whose
//! outcomes and trees were written from the TypeScript backend's sources and
//! which the TypeScript spec `fault-conformance.spec.ts` asserts against the
//! backend over the same seeded files, then run through the real filesystem
//! on the crashed root itself: the reopen takes the write lock, so a lock a
//! crashed holder kept fails it. Every case must be reached, so the table
//! holds exactly the reachable states.
//!
//! The error-path sweep fails each operation of each scenario once with each
//! error kind, and tears each write at half with each error kind: the
//! operation must report the error, the sources must be unchanged, and the
//! root must hold a state that some crash without a torn write also leaves,
//! or, for a torn temporary file, which a failed write leaves as TypeScript
//! leaves it, the state the torn crash at that write leaves; so a failed
//! append write is rolled back. A failed removal of a temporary file already
//! linked into place is swallowed instead. Each failed action is then
//! retried once, as a TypeScript caller retries a rejected batch, and the
//! scenario goes on: it must end with the fault-free run's logs, beside any
//! temporary file the failure left, except where a failed directory sync
//! after the link left the log published, which TypeScript's retry then
//! refuses to publish again. Each failure with an I/O error is also followed
//! by its retry and a crash or power cut at every later operation, and each
//! of those states must be a case too.
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
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bake_session::storage_io::{FileStat, ListedEntry, LockFailure, RealIo, StorageIo, StorageOp};
use bake_session::{
    LogCompression, LogFileRefusal, PathPlatform, PlainLogFile, compress_zstd_frame,
    js_string::{from_rust, to_rust},
};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "bake/session-conformance/fault-cases";
const ORACLE: &str = "in an owned temporary root holding the seeded files, a state a crashed or failed Session writer leaves, run each step through the JSONL backend with the case's compression, none unless it names zstd, on one handle: a write open, create, or the open handle's append, flush, or close; after each step list every file beneath the root with its text, or <hex BYTES> when its bytes are not UTF-8, and an empty session.lock by its size";
const VERSION: u64 = 3;
/// Both harnesses pin the table size, so a dropped case fails.
const CASE_COUNT: usize = 137;
const SOURCE_BUDGET: usize = 64;
/// The Zstd scenarios' plaintext bound, far above any of their logs.
const ZSTD: LogCompression = LogCompression::Zstd {
    max_plaintext_bytes: 1 << 20,
};
const LEASE_FILE: &str = "session.lock";
/// The temporary files' prefixes and suffixes, which TypeScript's
/// `materialize` and migration publication name; a Zstd log's come first,
/// so a plain prefix never claims them.
const TEMPORARIES: [(&str, &str); 4] = [
    ("session.v3.jsonl.zstd.", ".tmp"),
    ("session.v3.jsonl.", ".tmp"),
    ("session.migration.", ".jsonl.zstd.tmp"),
    ("session.migration.", ".jsonl.tmp"),
];
const PLATFORMS: [PathPlatform; 2] = [PathPlatform::Posix, PathPlatform::Win32];
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
    /// A crash, after which the root keeps only what a sync made durable.
    PowerCut,
    /// The operation does not run and reports the error; later ones run.
    Fail(Failure),
    /// A write stores its first `keep` bytes and reports the error; later
    /// operations run.
    TornFail { keep: usize, failure: Failure },
}

/// The faults of one run, by operation index.
type Plan = BTreeMap<usize, Fault>;

/// A node of the mirrored namespace: a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node {
    id: usize,
    dir: bool,
}

/// The namespace beneath the root and what syncs made durable of it.
#[derive(Debug, Default)]
struct Shadow {
    root: PathBuf,
    next: usize,
    /// Each current path's node; the root is node 0.
    names: BTreeMap<PathBuf, Node>,
    /// Each directory node's entries a sync or a write-through move made
    /// durable; a directory absent here has none.
    durable_entries: BTreeMap<usize, BTreeMap<OsString, Node>>,
    /// Each file node's bytes a sync made durable; a file absent here has
    /// none.
    durable_bytes: BTreeMap<usize, Vec<u8>>,
}

impl Shadow {
    /// The namespace of the seeded `root`, all of it durable.
    fn seeded(root: &Path) -> Self {
        let mut shadow = Self {
            root: root.to_path_buf(),
            next: 1,
            ..Self::default()
        };
        let root_node = Node { id: 0, dir: true };
        shadow.names.insert(root.to_path_buf(), root_node);
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let dir_node = shadow.names[&dir];
            let mut entries = BTreeMap::new();
            for entry in std::fs::read_dir(&dir).expect("list seed") {
                let entry = entry.expect("seed entry");
                let is_dir = entry.file_type().expect("seed type").is_dir();
                let node = shadow.node(is_dir);
                shadow.names.insert(entry.path(), node);
                entries.insert(entry.file_name(), node);
                if is_dir {
                    stack.push(entry.path());
                } else {
                    let bytes = std::fs::read(entry.path()).expect("seed bytes");
                    shadow.durable_bytes.insert(node.id, bytes);
                }
            }
            shadow.durable_entries.insert(dir_node.id, entries);
        }
        shadow
    }

    fn node(&mut self, dir: bool) -> Node {
        let id = self.next;
        self.next += 1;
        Node { id, dir }
    }

    fn inside(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }

    fn created_dirs(&mut self, dir: &Path) {
        if !self.inside(dir) {
            return;
        }
        let mut missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|ancestor| *ancestor != self.root)
            .filter(|ancestor| !self.names.contains_key(*ancestor))
            .map(Path::to_path_buf)
            .collect();
        missing.reverse();
        for path in missing {
            let node = self.node(true);
            self.names.insert(path, node);
        }
    }

    fn created_file(&mut self, path: &Path) {
        if self.inside(path) && !self.names.contains_key(path) {
            let node = self.node(false);
            self.names.insert(path.to_path_buf(), node);
        }
    }

    fn linked(&mut self, original: &Path, link: &Path) {
        if let Some(node) = self.names.get(original).copied() {
            self.names.insert(link.to_path_buf(), node);
        }
    }

    fn removed(&mut self, path: &Path) {
        self.names.remove(path);
    }

    /// A write-through move, which flushes NTFS's metadata journal: the
    /// move and every namespace change before it are durable at once.
    fn moved(&mut self, from: &Path, to: &Path) {
        let moved: Vec<(PathBuf, Node)> = self
            .names
            .iter()
            .filter(|(path, _)| path.starts_with(from))
            .map(|(path, node)| (path.clone(), *node))
            .collect();
        for (path, node) in moved {
            self.names.remove(&path);
            let rest = path.strip_prefix(from).expect("a moved path");
            let target = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            self.names.insert(target, node);
        }
        let dirs: Vec<PathBuf> = self
            .names
            .iter()
            .filter(|(_, node)| node.dir)
            .map(|(path, _)| path.clone())
            .collect();
        for dir in dirs {
            self.synced_dir(&dir);
        }
    }

    fn synced_file(&mut self, path: &Path) {
        if let Some(node) = self.names.get(path).copied() {
            let bytes = std::fs::read(path).expect("read a synced file");
            self.durable_bytes.insert(node.id, bytes);
        }
    }

    fn synced_dir(&mut self, dir: &Path) {
        let Some(node) = self.names.get(dir).copied() else {
            return;
        };
        let entries = self
            .names
            .iter()
            .filter(|(path, _)| path.parent() == Some(dir))
            .map(|(path, node)| {
                (
                    path.file_name().expect("an entry name").to_os_string(),
                    *node,
                )
            })
            .collect();
        self.durable_entries.insert(node.id, entries);
    }

    /// The nodes of the current namespace that have more than one path,
    /// each group relative to the root.
    fn link_groups(&self) -> Vec<Vec<PathBuf>> {
        let mut groups: BTreeMap<usize, Vec<PathBuf>> = BTreeMap::new();
        for (path, node) in &self.names {
            if !node.dir {
                let relative = path.strip_prefix(&self.root).expect("inside the root");
                groups
                    .entry(node.id)
                    .or_default()
                    .push(relative.to_path_buf());
            }
        }
        groups
            .into_values()
            .filter(|paths| paths.len() > 1)
            .collect()
    }

    /// Replace the root's contents with what survives a power cut, and
    /// mirror that namespace.
    fn power_loss(&mut self) {
        for entry in std::fs::read_dir(&self.root).expect("list the root") {
            let path = entry.expect("root entry").path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).expect("clear a directory");
            } else {
                std::fs::remove_file(&path).expect("clear a file");
            }
        }
        let mut names = BTreeMap::new();
        names.insert(self.root.clone(), Node { id: 0, dir: true });
        let mut first_path: BTreeMap<usize, PathBuf> = BTreeMap::new();
        let mut stack = vec![(self.root.clone(), 0usize)];
        while let Some((dir, id)) = stack.pop() {
            let entries = self.durable_entries.get(&id).cloned().unwrap_or_default();
            for (name, node) in entries {
                let path = dir.join(&name);
                names.insert(path.clone(), node);
                if node.dir {
                    std::fs::create_dir(&path).expect("rebuild a directory");
                    stack.push((path, node.id));
                } else if let Some(original) = first_path.get(&node.id) {
                    std::fs::hard_link(original, &path).expect("rebuild a link");
                } else {
                    let bytes = self
                        .durable_bytes
                        .get(&node.id)
                        .cloned()
                        .unwrap_or_default();
                    std::fs::write(&path, bytes).expect("rebuild a file");
                    first_path.insert(node.id, path);
                }
            }
        }
        self.names = names;
    }
}

#[derive(Debug, Default)]
struct FaultState {
    ops: Vec<StorageOp>,
    /// The bytes of each write, by operation index.
    writes: BTreeMap<usize, Vec<u8>>,
    crashed: bool,
    power_cut: bool,
    shadow: Shadow,
}

/// The real filesystem with deterministic faults and a durability model.
#[derive(Debug)]
struct FaultIo {
    platform: PathPlatform,
    plan: Plan,
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
    /// Faults over the seeded `root`, whose files are durable.
    fn new(platform: PathPlatform, plan: Plan, root: &Path) -> Arc<Self> {
        Arc::new(Self {
            platform,
            plan,
            state: Mutex::new(FaultState {
                shadow: Shadow::seeded(root),
                ..FaultState::default()
            }),
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

    fn crashed(&self) -> bool {
        self.state().crashed
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
        let Some(fault) = self.plan.get(&index).copied() else {
            return Gate::Run;
        };
        let torn = |keep: usize, error: io::Error| {
            if written.is_some() {
                Gate::Torn { keep, error }
            } else {
                Gate::Fail(error)
            }
        };
        match fault {
            Fault::Crash => {
                state.crashed = true;
                Gate::Fail(crashed())
            }
            Fault::PowerCut => {
                state.crashed = true;
                state.power_cut = true;
                Gate::Fail(crashed())
            }
            Fault::TornCrash { keep } => {
                state.crashed = true;
                torn(keep, crashed())
            }
            Fault::Fail(failure) => Gate::Fail(failure.error()),
            Fault::TornFail { keep, failure } => torn(keep, failure.error()),
        }
    }

    /// Run `real` under the plan, then record its effect in the model.
    fn run<T>(
        &self,
        op: StorageOp,
        real: impl FnOnce() -> io::Result<T>,
        mirror: impl FnOnce(&mut Shadow),
    ) -> io::Result<T> {
        match self.gate(op, None) {
            Gate::Run => {
                let value = real()?;
                mirror(&mut self.state().shadow);
                Ok(value)
            }
            Gate::Fail(error) | Gate::Torn { error, .. } => Err(error),
        }
    }

    /// After a power cut, rebuild the root from what was durable; a cut
    /// planned after the last operation cuts the finished run.
    fn finish(&self) {
        let mut state = self.state();
        let after_last = self
            .plan
            .iter()
            .any(|(&index, &fault)| fault == Fault::PowerCut && index >= state.ops.len());
        if state.power_cut || after_last {
            state.shadow.power_loss();
        }
    }

    fn link_groups(&self) -> Vec<Vec<PathBuf>> {
        self.state().shadow.link_groups()
    }
}

impl StorageIo for FaultIo {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.run(
            StorageOp::CreateDir,
            || RealIo.create_dir_all(dir),
            |shadow| shadow.created_dirs(dir),
        )
    }

    fn create_new(&self, path: &Path) -> io::Result<File> {
        self.run(
            StorageOp::CreateNew,
            || RealIo.create_new(path),
            |shadow| shadow.created_file(path),
        )
    }

    fn open_write(&self, path: &Path) -> io::Result<File> {
        self.run(StorageOp::OpenWrite, || RealIo.open_write(path), |_| {})
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
        self.run(StorageOp::SetLen, || RealIo.set_len(file, len), |_| {})
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.run(
            StorageOp::HardLink,
            || RealIo.hard_link(original, link),
            |shadow| shadow.linked(original, link),
        )
    }

    fn rename_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.run(
            StorageOp::RenameNew,
            || RealIo.rename_new(from, to),
            |shadow| shadow.moved(from, to),
        )
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.run(
            StorageOp::RemoveFile,
            || RealIo.remove_file(path),
            |shadow| shadow.removed(path),
        )
    }

    fn sync_file(&self, _file: &File, path: &Path) -> io::Result<()> {
        self.run(
            StorageOp::SyncFile,
            || Ok(()),
            |shadow| shadow.synced_file(path),
        )
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.run(
            StorageOp::SyncDir,
            || Ok(()),
            |shadow| shadow.synced_dir(dir),
        )
    }

    fn open_lock(&self, path: &Path) -> io::Result<File> {
        self.run(
            StorageOp::OpenLock,
            || RealIo.open_lock(path),
            |shadow| shadow.created_file(path),
        )
    }

    fn try_lock(&self, file: &File) -> Result<(), LockFailure> {
        match self.gate(StorageOp::TryLock, None) {
            Gate::Run => RealIo.try_lock(file),
            Gate::Fail(error) | Gate::Torn { error, .. } => Err(LockFailure::Io(error)),
        }
    }

    fn lock_is_current(&self, held: &File, path: &Path) -> io::Result<bool> {
        self.run(
            StorageOp::LockIsCurrent,
            || RealIo.lock_is_current(held, path),
            |_| {},
        )
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.run(StorageOp::Read, || RealIo.read(path), |_| {})
    }

    fn read_dir(&self, dir: &Path) -> io::Result<Option<Vec<ListedEntry>>> {
        self.run(StorageOp::ReadDir, || RealIo.read_dir(dir), |_| {})
    }

    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        self.run(
            StorageOp::Canonicalize,
            || RealIo.canonicalize(path),
            |_| {},
        )
    }

    fn probe(&self, path: &Path) -> io::Result<bool> {
        self.run(StorageOp::Probe, || RealIo.probe(path), |_| {})
    }

    fn stat_dir(&self, path: &Path) -> io::Result<Option<bool>> {
        self.run(StorageOp::StatDir, || RealIo.stat_dir(path), |_| {})
    }

    fn stat(&self, path: &Path, follow: bool) -> io::Result<FileStat> {
        self.run(StorageOp::Stat, || RealIo.stat(path, follow), |_| {})
    }

    fn write_platform(&self) -> PathPlatform {
        self.platform
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

/// A write scenario: the files it seeds, their bytes, and the actions it
/// runs with its compression.
#[derive(Debug)]
struct Scenario {
    name: &'static str,
    compression: LogCompression,
    seed: Vec<(&'static str, Vec<u8>)>,
    actions: Vec<Action>,
}

/// A file's listing: its text, or `<hex BYTES>` when it is not UTF-8.
fn listing(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => from_rust(&text).into_owned(),
        Err(bytes) => format!(
            "<hex {}>",
            bytes
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
    }
}

/// The bytes a [`listing`] stands for.
fn listed_bytes(listed: &str) -> Vec<u8> {
    match listed
        .strip_prefix("<hex ")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        Some(digits) => (0..digits.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&digits[index..index + 2], 16).expect("hex"))
            .collect(),
        None => to_rust(listed).as_bytes().to_vec(),
    }
}

/// `text`, a plain log, as a Zstd writer stores it: the header line as one
/// frame and the rows, when any, as another.
fn zstd_log(text: &str) -> Vec<u8> {
    let header_end = text.find('\n').expect("a header line") + 1;
    let mut frames = compress_zstd_frame(&text.as_bytes()[..header_end]).expect("frame");
    if header_end < text.len() {
        frames.extend(compress_zstd_frame(&text.as_bytes()[header_end..]).expect("frame"));
    }
    frames
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
    )
    .into_bytes();
    // A torn final frame missing two checksum bytes, which recovers its row.
    let recovered = compress_zstd_frame(line(&turn_end(1, 1)).as_bytes()).expect("frame");
    let zstd_torn = [zstd_log(&stored), recovered[..recovered.len() - 2].to_vec()].concat();
    let zstd_stored = zstd_log(&stored);
    let stored = stored.into_bytes();
    let mut list = vec![
        Scenario {
            compression: LogCompression::None,
            name: "create-flush",
            seed: vec![],
            actions: vec![Action::Create(header("w")), Action::Flush],
        },
        Scenario {
            compression: LogCompression::None,
            name: "create-append",
            seed: vec![],
            actions: vec![
                Action::Create(header("w")),
                Action::Append(vec![turn_start(0, 1), turn_end(1, 1)]),
            ],
        },
        Scenario {
            compression: LogCompression::None,
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
            compression: LogCompression::None,
            name: "open-append",
            seed: vec![("_no-cwd/w/session.v3.jsonl", stored)],
            actions: vec![
                Action::Open("w"),
                Action::Append(vec![turn_end(1, 1), turn_start(2, 2)]),
            ],
        },
        Scenario {
            compression: LogCompression::None,
            name: "open-torn-append",
            seed: vec![("_no-cwd/w/session.v3.jsonl", torn)],
            actions: vec![Action::Open("w"), Action::Append(vec![turn_end(1, 1)])],
        },
        Scenario {
            compression: ZSTD,
            name: "zstd-create-flush-appends",
            seed: vec![],
            actions: vec![
                Action::Create(header("w")),
                Action::Flush,
                Action::Append(vec![turn_start(0, 1)]),
                Action::Append(vec![turn_end(1, 1)]),
            ],
        },
        Scenario {
            compression: ZSTD,
            name: "zstd-create-append",
            seed: vec![],
            actions: vec![
                Action::Create(header("w")),
                Action::Append(vec![turn_start(0, 1), turn_end(1, 1)]),
            ],
        },
        Scenario {
            compression: ZSTD,
            name: "zstd-open-append",
            seed: vec![("_no-cwd/w/session.v3.jsonl.zstd", zstd_stored)],
            actions: vec![
                Action::Open("w"),
                Action::Append(vec![turn_end(1, 1), turn_start(2, 2)]),
            ],
        },
        Scenario {
            compression: ZSTD,
            name: "zstd-open-torn-append",
            seed: vec![("_no-cwd/w/session.v3.jsonl.zstd", zstd_torn)],
            actions: vec![Action::Open("w"), Action::Append(vec![turn_start(2, 2)])],
        },
    ];
    for (name, case, id, compression) in [
        (
            "migrate-v0",
            "migrate-v0-packed-run",
            "hist",
            LogCompression::None,
        ),
        (
            "migrate-v1",
            "migrate-v1-packed-run",
            "hist",
            LogCompression::None,
        ),
        (
            "migrate-v2",
            "migrate-v2-seeded",
            "v2v3",
            LogCompression::None,
        ),
        ("zstd-migrate-v2", "migrate-v2-seeded", "v2v3", ZSTD),
    ] {
        let (file, text) = released_seed(case);
        let (file, bytes) = if compression == LogCompression::None {
            (file, to_rust(&text).as_bytes().to_vec())
        } else {
            (format!("{file}.zstd"), zstd_log(&to_rust(&text)))
        };
        let file: &'static str = Box::leak(file.into_boxed_str());
        list.push(Scenario {
            name,
            compression,
            seed: vec![(file, bytes)],
            actions: vec![Action::Open(id), Action::Flush],
        });
    }
    list
}

/// How a scenario's actions ended.
#[derive(Debug, Default)]
struct Ran {
    /// The first refusal.
    first: Option<LogFileRefusal>,
    /// The refusal that stopped the actions, as text.
    last: Option<String>,
    /// Whether every action, or its retry, succeeded.
    completed: bool,
    /// When recorded, the operation count and the logs after each action
    /// that succeeded: what a power cut from then on must keep.
    acknowledged: Vec<(usize, BTreeMap<String, String>)>,
}

/// Run the scenario's actions through `io` until one fails, then drop the
/// handle as a dead process would. With `retry`, a failed action is run once
/// more unless the process crashed, as a TypeScript caller retries a
/// rejected operation, and the scenario goes on when the retry succeeds.
fn run_actions(
    io: &Arc<FaultIo>,
    root: &Path,
    scenario: &Scenario,
    retry: bool,
    record: bool,
) -> Ran {
    let shared: Arc<dyn StorageIo> = io.clone();
    let mut handle: Option<PlainLogFile> = None;
    let mut ran = Ran::default();
    for action in &scenario.actions {
        let mut attempt = || match action {
            Action::Create(header) => PlainLogFile::create_compressed_with_io(
                shared.clone(),
                root,
                header,
                None,
                scenario.compression,
            )
            .map(|created| handle = Some(created)),
            Action::Open(id) => PlainLogFile::open_compressed_with_io(
                shared.clone(),
                root,
                id,
                SOURCE_BUDGET,
                scenario.compression,
            )
            .map(|opened| handle = Some(opened)),
            Action::Append(events) => handle.as_mut().expect("a handle").append(events),
            Action::Flush => handle.as_mut().expect("a handle").flush(),
        };
        let mut outcome = attempt();
        if retry && !io.crashed() && outcome.is_err() {
            ran.first = ran.first.or(outcome.err());
            outcome = attempt();
        }
        if let Err(refusal) = outcome {
            ran.last = Some(refusal.to_string_lossy());
            ran.first = ran.first.or(Some(refusal));
            drop(handle);
            io.finish();
            return ran;
        }
        if record {
            ran.acknowledged.push((io.ops().len(), logs(tree(root))));
        }
    }
    ran.completed = true;
    drop(handle);
    io.finish();
    ran
}

fn seed_root(root: &Path, scenario: &Scenario) {
    for (file, bytes) in &scenario.seed {
        let path = root.join(&*to_rust(file));
        std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed directory");
        std::fs::write(&path, bytes).expect("seed file");
    }
}

/// Every seeded older generation must be byte-identical; the current
/// generation a scenario seeds is the log it appends to.
fn check_sources(root: &Path, scenario: &Scenario, context: &str) {
    for (file, seeded) in &scenario.seed {
        if file.ends_with("/session.v3.jsonl") || file.ends_with("/session.v3.jsonl.zstd") {
            continue;
        }
        let bytes = std::fs::read(root.join(&*to_rust(file))).expect("source generation");
        assert!(bytes == *seeded, "{context}: source {file} changed");
    }
}

/// A temporary file's prefix, token, and suffix, or `None` for any other
/// name.
fn temporary_parts(name: &str) -> Option<(&'static str, &str, &'static str)> {
    TEMPORARIES.into_iter().find_map(|(prefix, suffix)| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .filter(|token| !token.is_empty() && !token.contains('/'))
            .map(|token| (prefix, token, suffix))
    })
}

/// The creation order of a token: Rust's `<pid>-<counter>` by its counter,
/// a normalized token by its number.
fn token_order(token: &str) -> (u64, String) {
    let counter = token.rsplit('-').next().unwrap_or(token);
    (counter.parse().unwrap_or(u64::MAX), token.to_owned())
}

/// Every file beneath `root`, a `session.lock` by its size and each
/// temporary file under its normalized name, and the normalized relative
/// path of every listed file.
fn snapshot(root: &Path) -> (BTreeMap<String, String>, BTreeMap<PathBuf, String>) {
    fn walk(
        dir: &Path,
        relative: &Path,
        prefix: &str,
        files: &mut BTreeMap<String, String>,
        names: &mut BTreeMap<PathBuf, String>,
    ) {
        let mut listed = Vec::new();
        for entry in std::fs::read_dir(dir).expect("list") {
            let entry = entry.expect("entry");
            listed.push((
                entry.file_name(),
                entry.file_type().expect("file type").is_dir(),
            ));
        }
        // Temporaries are numbered by kind in their creation order.
        // Each kind's temporaries: creation order, token, and name.
        type Found = Vec<(u64, String, String)>;
        let mut temporaries: BTreeMap<(&str, &str), Found> = BTreeMap::new();
        for (raw, is_dir) in &listed {
            let name = from_rust(&raw.to_string_lossy()).into_owned();
            if !is_dir && let Some((temp_prefix, token, suffix)) = temporary_parts(&name) {
                let (order, token) = token_order(token);
                temporaries.entry((temp_prefix, suffix)).or_default().push((
                    order,
                    token,
                    name.clone(),
                ));
            }
        }
        let mut renamed = BTreeMap::new();
        for ((temp_prefix, suffix), mut found) in temporaries {
            found.sort();
            for (index, (_, _, name)) in found.into_iter().enumerate() {
                renamed.insert(name, format!("{temp_prefix}{index:012}{suffix}"));
            }
        }
        for (raw, is_dir) in listed {
            let name = from_rust(&raw.to_string_lossy()).into_owned();
            let path = dir.join(&raw);
            if is_dir {
                walk(
                    &path,
                    &relative.join(&raw),
                    &format!("{prefix}{name}/"),
                    files,
                    names,
                );
                continue;
            }
            let text = if name == LEASE_FILE {
                let size = std::fs::metadata(&path).expect("lock metadata").len();
                if size == 0 {
                    String::new()
                } else {
                    format!("<{size} bytes>")
                }
            } else {
                listing(std::fs::read(&path).expect("read a file"))
            };
            let normalized = renamed.get(&name).cloned().unwrap_or(name);
            let key = format!("{prefix}{normalized}");
            names.insert(relative.join(&raw), key.clone());
            files.insert(key, text);
        }
    }
    let mut files = BTreeMap::new();
    let mut names = BTreeMap::new();
    walk(root, Path::new(""), "", &mut files, &mut names);
    (files, names)
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    snapshot(root).0
}

/// A state a run leaves: its files and its hard-linked groups of them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    files: BTreeMap<String, String>,
    links: BTreeSet<BTreeSet<String>>,
}

/// The state of a root `io` ran over.
fn state(root: &Path, io: &FaultIo) -> State {
    let (files, names) = snapshot(root);
    let links = io
        .link_groups()
        .into_iter()
        .map(|group| {
            group
                .iter()
                .map(|path| names.get(path).expect("a linked file is listed").clone())
                .collect()
        })
        .collect();
    State { files, links }
}

// ---------------------------------------------------------------------------
// The table.

struct Case {
    id: String,
    compression: LogCompression,
    scenarios: Vec<String>,
    /// Each seeded file's text and its hard-linked groups.
    seed: State,
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
    assert_eq!(table["version"], VERSION);
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
            let allowed =
                BTreeSet::from(["compression", "id", "note", "scenarios", "seed", "steps"]);
            assert!(keys(entry).is_subset(&allowed), "{id}: keys");
            let compression = match entry.get("compression") {
                None => LogCompression::None,
                Some(name) => {
                    assert_eq!(text(name, &id), "zstd", "{id}: compression");
                    ZSTD
                }
            };
            let scenarios = entry["scenarios"]
                .as_array()
                .expect("scenarios")
                .iter()
                .map(|name| text(name, &id).to_owned())
                .collect();
            let mut files = BTreeMap::new();
            let mut links = BTreeMap::new();
            for file in entry["seed"].as_array().expect("seed") {
                let file = object(file, &id);
                let path = text(&file["file"], &id).to_owned();
                if let Some(target) = file.get("hardLinkTo") {
                    assert_eq!(keys(file), BTreeSet::from(["file", "hardLinkTo"]), "{id}");
                    links.insert(path, text(target, &id).to_owned());
                } else if let Some(bytes) = file.get("hex") {
                    assert_eq!(keys(file), BTreeSet::from(["file", "hex"]), "{id}: seed");
                    files.insert(path, format!("<hex {}>", text(bytes, &id)));
                } else {
                    assert_eq!(keys(file), BTreeSet::from(["file", "text"]), "{id}: seed");
                    files.insert(path, text(&file["text"], &id).to_owned());
                }
            }
            let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for (link, target) in &links {
                let target_text = files.get(target).expect("a seeded link target").clone();
                files.insert(link.clone(), target_text);
                let group = groups.entry(target.clone()).or_default();
                group.insert(target.clone());
                group.insert(link.clone());
            }
            let steps = entry["steps"]
                .as_array()
                .expect("steps")
                .iter()
                .map(|step| object(step, &id).clone())
                .collect();
            Case {
                id,
                compression,
                scenarios,
                seed: State {
                    files,
                    links: groups.into_values().collect(),
                },
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

/// `tree` without the lock and temporary files: the logs.
fn logs(tree: BTreeMap<String, String>) -> BTreeMap<String, String> {
    without_temporaries(tree)
        .into_iter()
        .filter(|(name, _)| !name.ends_with(&format!("/{LEASE_FILE}")))
        .collect()
}

/// `tree` without temporary files.
fn without_temporaries(tree: BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.into_iter()
        .filter(|(name, _)| temporary_parts(name.rsplit('/').next().unwrap_or_default()).is_none())
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
            "open" => PlainLogFile::open_compressed(
                root,
                text(&step["id"], &context),
                SOURCE_BUDGET,
                case.compression,
            )
            .map(|opened| handle = Some(opened)),
            "create" => {
                PlainLogFile::create_compressed(root, &step["header"], None, case.compression)
                    .map(|created| handle = Some(created))
            }
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
fn state_case<'a>(cases: &'a [Case], scenario: &str, state: &State) -> Option<&'a Case> {
    cases
        .iter()
        .find(|case| case.scenarios.iter().any(|name| name == scenario) && case.seed == *state)
}

/// The states a sweep reached, the cases they matched, and the states no
/// case seeds.
#[derive(Debug, Default)]
struct Reached {
    /// Each reached case's id and the scenario that reached it.
    cases: BTreeSet<(String, String)>,
    unknown: Vec<String>,
}

impl Reached {
    fn merge(&mut self, other: Self) {
        self.cases.extend(other.cases);
        self.unknown.extend(other.unknown);
    }
}

/// One run of `scenario` under `plan` in a fresh root: the sources must be
/// unchanged and the state a case of the scenario, whose steps then run over
/// that root. Returns how the actions ended and the state.
fn faulted_run(
    cases: &[Case],
    scenario: &Scenario,
    platform: PathPlatform,
    plan: &Plan,
    retry: bool,
    reached: &mut Reached,
) -> (Ran, State) {
    let scratch = Scratch::new();
    seed_root(&scratch.0, scenario);
    let io = FaultIo::new(platform, plan.clone(), &scratch.0);
    let ran = run_actions(&io, &scratch.0, scenario, retry, false);
    let context = format!("{} {platform:?} {plan:?} retry {retry}", scenario.name);
    check_sources(&scratch.0, scenario, &context);
    let state = state(&scratch.0, &io);
    match state_case(cases, scenario.name, &state) {
        Some(case) => {
            reached
                .cases
                .insert((case.id.clone(), scenario.name.to_owned()));
            run_case(case, &scratch.0, Strictness::Exact);
        }
        None => {
            let links: Vec<&BTreeSet<String>> = state.links.iter().collect();
            let line = json!({
                "scenario": scenario.name,
                "platform": format!("{platform:?}"),
                "fault": format!("{plan:?} retry {retry}"),
                "state": state.files,
                "links": links,
            });
            if let Ok(path) = std::env::var("BAKE_FAULT_DUMP") {
                use std::io::Write;
                static DUMP: Mutex<()> = Mutex::new(());
                let _serial = DUMP.lock().unwrap_or_else(PoisonError::into_inner);
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)
                    .expect("dump");
                writeln!(file, "{line}").expect("dump");
            }
            reached.unknown.push(format!(
                "{context}: scenario {} reached a state no case seeds: {line}",
                scenario.name
            ));
        }
    }
    (ran, state)
}

/// A run's operations, the bytes of its writes, and its state.
struct Counted {
    ops: Vec<StorageOp>,
    writes: BTreeMap<usize, Vec<u8>>,
    ran: Ran,
    state: State,
}

/// Run `scenario` under `plan` without checking the state against the table.
fn count(scenario: &Scenario, platform: PathPlatform, plan: &Plan, retry: bool) -> Counted {
    let scratch = Scratch::new();
    seed_root(&scratch.0, scenario);
    let io = FaultIo::new(platform, plan.clone(), &scratch.0);
    let ran = run_actions(&io, &scratch.0, scenario, retry, plan.is_empty());
    let state = state(&scratch.0, &io);
    Counted {
        ops: io.ops(),
        writes: io.writes(),
        ran,
        state,
    }
}

/// The fault-free run, which must succeed.
fn clean(scenario: &Scenario, platform: PathPlatform) -> Counted {
    let counted = count(scenario, platform, &Plan::new(), false);
    assert!(
        counted.ran.completed,
        "{} {platform:?}: {:?}",
        scenario.name, counted.ran.first
    );
    counted
}

/// Whether operation `index` removes a temporary file already linked into
/// place, after the link and the directory sync that follows it.
fn published_temporary_removal(ops: &[StorageOp], index: usize) -> bool {
    ops[index] == StorageOp::RemoveFile
        && ops[..index]
            .iter()
            .rev()
            .find(|op| **op != StorageOp::SyncDir)
            == Some(&StorageOp::HardLink)
}

/// Whether write `index` writes a temporary file.
fn temporary_write(ops: &[StorageOp], index: usize) -> bool {
    ops[index] == StorageOp::WriteAt && index > 0 && ops[index - 1] == StorageOp::CreateNew
}

/// Whether operation `index` syncs a directory a log was just linked into,
/// so a failure there leaves that log published.
fn sync_after_link(ops: &[StorageOp], index: usize) -> bool {
    ops[index] == StorageOp::SyncDir && index > 0 && ops[index - 1] == StorageOp::HardLink
}

fn plan(index: usize, fault: Fault) -> Plan {
    Plan::from([(index, fault)])
}

/// Every crash point, torn crash, and power cut of `scenario` on `platform`.
fn crash_sweep(cases: &[Case], scenario: &Scenario, platform: PathPlatform) -> Reached {
    let mut reached = Reached::default();
    let Counted {
        ops, writes, ran, ..
    } = clean(scenario, platform);
    // Every operation that returned keeps its logs through a power cut.
    for (boundary, acknowledged) in &ran.acknowledged {
        let (_, state) = faulted_run(
            cases,
            scenario,
            platform,
            &plan(*boundary, Fault::PowerCut),
            false,
            &mut reached,
        );
        assert_eq!(
            &logs(state.files),
            acknowledged,
            "{} {platform:?}: a power cut at {boundary} lost an acknowledged write",
            scenario.name
        );
    }
    assert!(
        ops.contains(&StorageOp::TryLock),
        "{} {platform:?}: no lock taken",
        scenario.name
    );
    for index in 0..=ops.len() {
        for fault in [Fault::Crash, Fault::PowerCut] {
            let (ran, _) = faulted_run(
                cases,
                scenario,
                platform,
                &plan(index, fault),
                false,
                &mut reached,
            );
            // A crash at the removal of a published temporary is swallowed,
            // as TypeScript swallows a failed removal there, and only a
            // later operation reports it.
            if index == ops.len() || !published_temporary_removal(&ops, index) {
                assert_eq!(
                    ran.first.is_some(),
                    index < ops.len(),
                    "{} {platform:?} {index} {fault:?}",
                    scenario.name
                );
            }
        }
        if let Some(bytes) = writes.get(&index) {
            let fault = Fault::TornCrash {
                keep: bytes.len() / 2,
            };
            faulted_run(
                cases,
                scenario,
                platform,
                &plan(index, fault),
                false,
                &mut reached,
            );
        }
    }
    reached
}

/// Every failed operation of `scenario` on `platform`; see the module
/// comment.
fn failure_sweep(cases: &[Case], scenario: &Scenario, platform: PathPlatform) -> Reached {
    let mut reached = Reached::default();
    let fault_free = clean(scenario, platform);
    let (ops, writes) = (&fault_free.ops, &fault_free.writes);
    let crash_states: Vec<State> = (0..=ops.len())
        .map(|index| count(scenario, platform, &plan(index, Fault::Crash), false).state)
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
            let context = format!("{} {platform:?} {index} {fault:?}", scenario.name);
            let failed = plan(index, fault);
            let (ran, state) = faulted_run(cases, scenario, platform, &failed, false, &mut reached);
            // A failed removal of a published temporary is swallowed, as
            // TypeScript's is, and the run goes on with that second link.
            if published_temporary_removal(ops, index) {
                assert!(ran.completed, "{context}: {:?}", ran.first);
            } else {
                let torn_temporary =
                    matches!(fault, Fault::TornFail { .. }) && temporary_write(ops, index);
                let allowed = crash_states.contains(&state)
                    || torn_temporary && {
                        let keep = writes[&index].len() / 2;
                        state
                            == count(
                                scenario,
                                platform,
                                &plan(index, Fault::TornCrash { keep }),
                                false,
                            )
                            .state
                    };
                assert!(allowed, "{context}: a state no crash leaves: {state:?}");
                assert!(
                    matches!(ran.first, Some(LogFileRefusal::Io(_))),
                    "{context}: {:?}",
                    ran.first
                );
            }
            // The failed action's retry, and every later action.
            let (ran, state) = faulted_run(cases, scenario, platform, &failed, true, &mut reached);
            if sync_after_link(ops, index)
                && scenario
                    .name
                    .trim_start_matches("zstd-")
                    .starts_with("create")
            {
                assert!(
                    ran.last
                        .as_ref()
                        .is_some_and(|last| last.contains("refusing to materialize")),
                    "{context}: {:?}",
                    ran.last
                );
                continue;
            }
            assert!(ran.completed, "{context}: the retry failed: {:?}", ran.last);
            assert_eq!(
                without_temporaries(state.files),
                without_temporaries(fault_free.state.files.clone()),
                "{context}: the retried run's logs"
            );
        }
    }
    // A failure, its retry, and a crash or power cut at every later point.
    // A failed `stat` of a migration leaves the disk state a failure of the
    // operation beside it leaves: the read before or after it, the
    // read-back of the temporary file, or the directory sync after the
    // link, each of which removes the temporary file as it does. The retry
    // is a new write `open` of that state, so its later points are swept
    // there. Likewise a failed read-only operation directly followed by
    // another, such as the second `findLog` under the lock, leaves the disk
    // state and refusal path a failure of the next one leaves, so only the
    // last of such a run is swept.
    let read_only = |op: &StorageOp| {
        matches!(
            op,
            StorageOp::Read
                | StorageOp::ReadDir
                | StorageOp::Canonicalize
                | StorageOp::Probe
                | StorageOp::StatDir
                | StorageOp::Stat
        )
    };
    for (index, op) in ops.iter().enumerate() {
        if *op == StorageOp::Stat || read_only(op) && ops.get(index + 1).is_some_and(read_only) {
            continue;
        }
        let mut faults = vec![Fault::Fail(Failure::Eio)];
        if let Some(bytes) = writes.get(&index) {
            faults.push(Fault::TornFail {
                keep: bytes.len() / 2,
                failure: Failure::Eio,
            });
        }
        for fault in faults {
            let failed = plan(index, fault);
            let total = count(scenario, platform, &failed, true).ops.len();
            for later in index + 1..=total {
                for cut in [Fault::Crash, Fault::PowerCut] {
                    let mut both = failed.clone();
                    both.insert(later, cut);
                    faulted_run(cases, scenario, platform, &both, true, &mut reached);
                }
            }
        }
    }
    reached
}

/// Run `sweep` over every scenario and platform, in parallel.
fn sweep_all(sweep: fn(&[Case], &Scenario, PathPlatform) -> Reached) -> Reached {
    let cases = load();
    let scenarios = scenarios();
    let jobs: Vec<(&Scenario, PathPlatform)> = scenarios
        .iter()
        .flat_map(|scenario| PLATFORMS.map(|platform| (scenario, platform)))
        .collect();
    let mut reached = Reached::default();
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|&(scenario, platform)| {
                let cases = &cases;
                scope.spawn(move || sweep(cases, scenario, platform))
            })
            .collect();
        for handle in handles {
            reached.merge(handle.join().expect("a sweep thread"));
        }
    });
    reached
}

#[test]
fn every_crash_point_leaves_a_state_typescript_reopens_alike() {
    let mut reached = sweep_all(crash_sweep);
    reached.merge(sweep_all(failure_sweep));
    assert!(reached.unknown.is_empty(), "{}", reached.unknown.join("\n"));
    let all: BTreeSet<(String, String)> = load()
        .into_iter()
        .flat_map(|case| {
            case.scenarios
                .into_iter()
                .map(move |scenario| (case.id.clone(), scenario))
        })
        .collect();
    let unreached: Vec<&(String, String)> = all.difference(&reached.cases).collect();
    assert!(
        unreached.is_empty(),
        "cases a listed scenario does not reach: {unreached:?}"
    );
}

/// When a failed append's rollback fails too, the file holds bytes the
/// model does not, which TypeScript's retry would append after; the value
/// refuses every later operation instead.
#[test]
fn a_failed_rollback_refuses_every_later_operation() {
    let scenario = scenarios()
        .into_iter()
        .find(|scenario| scenario.name == "open-append")
        .expect("open-append");
    for platform in PLATFORMS {
        let Counted { writes, .. } = clean(&scenario, platform);
        let (&write, bytes) = writes.iter().last().expect("the append write");
        // The faulted run fails the write, then the rollback opens the file
        // (`write + 1`) and truncates it (`write + 2`); the clean run has no
        // rollback, so the index is counted from the faulted sequence.
        let truncate = write + 2;
        let scratch = Scratch::new();
        seed_root(&scratch.0, &scenario);
        let keep = bytes.len() / 2;
        let both = Plan::from([
            (
                write,
                Fault::TornFail {
                    keep,
                    failure: Failure::Eio,
                },
            ),
            (truncate, Fault::Fail(Failure::Eio)),
        ]);
        let io = FaultIo::new(platform, both, &scratch.0);
        let shared: Arc<dyn StorageIo> = io.clone();
        let mut handle =
            PlainLogFile::open_with_io(shared, &scratch.0, "w", SOURCE_BUDGET).expect("open");
        let Action::Append(events) = &scenario.actions[1] else {
            panic!("an append");
        };
        let failed = handle.append(events).expect_err("the torn append");
        assert!(
            failed.to_string_lossy().contains("failed to roll back"),
            "{platform:?}: {failed:?}"
        );
        assert_eq!(
            io.ops().get(truncate),
            Some(&StorageOp::SetLen),
            "{platform:?}: the second fault lands on the rollback's truncation"
        );
        for retried in [handle.append(events), handle.flush()] {
            let refusal = retried.expect_err("a diverged value refuses");
            assert!(
                refusal
                    .to_string_lossy()
                    .contains("could not be rolled back"),
                "{platform:?}: {refusal:?}"
            );
        }
        let log = std::fs::read(scratch.0.join("_no-cwd/w/session.v3.jsonl")).expect("log");
        let (_, stored) = &scenario.seed[0];
        assert_eq!(
            log.len(),
            stored.len() + keep,
            "{platform:?}: the torn rows stay"
        );
    }
}

trait RefusalText {
    fn to_string_lossy(&self) -> String;
}

impl RefusalText for LogFileRefusal {
    fn to_string_lossy(&self) -> String {
        match self {
            LogFileRefusal::Io(error) => error.to_string(),
            other => format!("{other:?}"),
        }
    }
}

/// Every write torn at every byte count, crashing; see the module comment.
#[test]
#[ignore = "the full torn-write sweep; the default sweeps tear each write at half"]
fn full_torn_write_sweep() {
    let cases = load();
    for scenario in scenarios() {
        for platform in PLATFORMS {
            let Counted { ops, writes, .. } = clean(&scenario, platform);
            for (&index, bytes) in &writes {
                let half = count(
                    &scenario,
                    platform,
                    &plan(
                        index,
                        Fault::TornCrash {
                            keep: bytes.len() / 2,
                        },
                    ),
                    false,
                )
                .state;
                let reference = state_case(&cases, scenario.name, &half).unwrap_or_else(|| {
                    panic!(
                        "{} {platform:?} {index}: the half-torn state",
                        scenario.name
                    )
                });
                for keep in 0..bytes.len() {
                    let scratch = Scratch::new();
                    seed_root(&scratch.0, &scenario);
                    let io =
                        FaultIo::new(platform, plan(index, Fault::TornCrash { keep }), &scratch.0);
                    run_actions(&io, &scratch.0, &scenario, false, false);
                    let context = format!(
                        "{} {platform:?} {index} keep {keep} of {:?}",
                        scenario.name, ops[index]
                    );
                    check_sources(&scratch.0, &scenario, &context);
                    for name in tree(&scratch.0).keys() {
                        let base = name.rsplit('/').next().unwrap_or_default();
                        assert!(
                            base == LEASE_FILE
                                || temporary_parts(base).is_some()
                                || canonical_generation_name(base),
                            "{context}: stray {name}"
                        );
                    }
                    // A torn temporary is never read, so any cut reopens as
                    // the half cut does. A torn log append reopens alike only
                    // when the cut leaves as many whole rows as the half cut,
                    // and not at a line end, which TypeScript's scan reads as
                    // no torn tail.
                    let rows =
                        |cut: usize| bytes[..cut].iter().filter(|&&byte| byte == b'\n').count();
                    let ends_line = keep > 0 && bytes[keep - 1] == b'\n';
                    // A torn frame's rows are counted only by decoding it,
                    // so a Zstd log append compares the open alone.
                    let plain_log = scenario.compression == LogCompression::None;
                    let strictness = if temporary_write(&ops, index)
                        || (plain_log && !ends_line && rows(keep) == rows(bytes.len() / 2))
                    {
                        Strictness::FinalLog
                    } else {
                        Strictness::OpenOnly
                    };
                    run_case(reference, &scratch.0, strictness);
                }
            }
        }
    }
}

/// Whether `name` is a canonical plain generation's name.
fn canonical_generation_name(name: &str) -> bool {
    matches!(
        name.strip_suffix(".zstd").unwrap_or(name),
        "session.jsonl" | "session.v1.jsonl" | "session.v2.jsonl" | "session.v3.jsonl"
    )
}

#[test]
fn shared_cases_reopen_like_the_typescript_backend() {
    for case in load() {
        let scratch = Scratch::new();
        for (file, file_text) in &case.seed.files {
            if case.links.contains_key(file) {
                continue;
            }
            let path = scratch.0.join(&*to_rust(file));
            std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed dir");
            std::fs::write(&path, listed_bytes(file_text)).expect("seed file");
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

/// On Windows the write sequence's publication is a real `MoveFileExW`
/// with write-through, which refuses an existing destination, and a
/// published log leaves no temporary file.
#[cfg(windows)]
#[test]
fn windows_publication_moves_without_replacing() {
    let scratch = Scratch::new();
    let (from, to) = (scratch.0.join("from.tmp"), scratch.0.join("to"));
    std::fs::write(&from, "staged").expect("stage");
    std::fs::write(&to, "kept").expect("target");
    let refused = RealIo
        .rename_new(&from, &to)
        .expect_err("an existing target");
    assert_eq!(refused.kind(), ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&to).expect("target"), "kept");
    std::fs::remove_file(&to).expect("clear the target");
    RealIo.rename_new(&from, &to).expect("move");
    assert_eq!(std::fs::read_to_string(&to).expect("moved"), "staged");
    assert!(!from.exists());

    let root = Scratch::new();
    let mut handle = PlainLogFile::create(&root.0, &header("w"), None).expect("create");
    handle.flush().expect("flush");
    drop(handle);
    assert_eq!(
        tree(&root.0),
        BTreeMap::from([
            ("_no-cwd/w/session.lock".to_owned(), String::new()),
            ("_no-cwd/w/session.v3.jsonl".to_owned(), header_line("w")),
        ])
    );
}
