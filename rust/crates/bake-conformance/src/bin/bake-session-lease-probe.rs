//! Development-only Session write-lease probe for cross-runtime tests, not an
//! agent. It drives one public `bake_session::PlainLogFile` handle per process
//! so tests can contend for a Session's write lock against another process,
//! and resume a log another process wrote or left torn.
//!
//! - `hold <absolute-root> <id>` creates a Session with no `cwd`, appends
//!   `turn/start` seq 0 and `turn/end` seq 1, which takes the write lock, then
//!   prints `{"state":"holding"}` and keeps the handle while reading stdin.
//! - `hold-open <absolute-root> <id>` write-opens an existing plain Session,
//!   appends nothing, then holds it as `hold` does.
//! - `open <absolute-root> <id> <seq>` write-opens the Session. When another
//!   handle owns it, prints `{"outcome":"owned","message":...}` with
//!   `SessionAlreadyOwnedError`'s exact message and exits 3. Otherwise it
//!   appends `turn/start` at `seq`, closes the handle, scans the file, and
//!   prints `{"outcome":"opened","seqs":[...]}` with every stored seq. Any
//!   other refusal that carries TypeScript's exact message prints
//!   `{"outcome":"refused","message":...}`, the message as `JSON.stringify`
//!   spells it, and exits 1; an I/O failure prints
//!   `{"outcome":"io","kind":...}`, the `std::io::ErrorKind` by its `Debug`
//!   name, such as `PermissionDenied`, and exits 1.
//! - `pause-migrate <absolute-root> <id> <publish|unlink> <seq>` runs `open`
//!   on a Session whose newest generation is v0, v1, or v2, through the
//!   `storage_io` seam, which pauses the migration once: `publish` before it
//!   creates its `session.migration.*` temporary file, after the source was
//!   read and migrated and before anything is published, and `unlink` after
//!   the temporary file was linked into place and the directory synced,
//!   before the temporary file is removed (POSIX only, since Win32 moves it).
//!   Paused, it prints `{"state":"paused"}`, holding the write lock; the
//!   stdin line `go` resumes it, after which it reports as `open` does, and
//!   stdin EOF exits 0 without resuming, so killing it is a writer killed at
//!   that step. Reaching no pause is a fixture error.
//! - `tear-create <absolute-root> <id> <bytes>` creates the Session as `hold`
//!   does, and `tear-append <absolute-root> <id> <seq> <bytes>` write-opens
//!   it and appends `turn/start` at `seq` and `turn/end` at `seq + 1`. Each
//!   makes every filesystem operation through the hidden
//!   `bake_session::storage_io` seam, which passes them to the real
//!   filesystem until the handle's writes reach `bytes` bytes in all: the
//!   write that reaches the budget stores only its bytes up to it, then
//!   prints `{"state":"torn"}` and never returns, holding the write lock and
//!   leaving every cleanup unrun, so killing the process is a writer killed
//!   mid-flush. At stdin EOF it exits 0 without returning. Writes that end
//!   within the budget are a fixture error. When the Session's newest
//!   generation is v0, v1, or v2, the write `open` migrates it first, so the
//!   budget counts the migration's temporary file first, and a budget below
//!   its size leaves a writer killed while writing it.
//!
//! A holder releases on the exact stdin line `release`, printing
//! `{"state":"released"}` after the handle is dropped, and on stdin EOF
//! silently; both exit 0. Any other stdin input is a fixture error. No
//! signal handler is installed, so a killed holder never runs `Drop`.
//!
//! Exit codes: 0 success, 1 any refusal or I/O failure other than ownership,
//! 2 usage or fixture error, 3 owned. A refused `open` with TypeScript's
//! message writes only to stdout; every other failure only to stderr.

use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bake_session::storage_io::{FileStat, ListedEntry, LockFailure, RealIo, StorageIo};
use bake_session::{LogFileRefusal, PathPlatform, PlainLogFile, json_text, scan_log};
use serde_json::{Value, json};

/// Bounds each expanded `sourceEventSeqs` field, as the Session tests do.
const SOURCE_BUDGET: usize = 64;
/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
/// The longest stdin line a holder reads, LF included.
const MAX_COMMAND_BYTES: u64 = 64;
const OWNED_EXIT: u8 = 3;

enum Failure {
    /// Bad arguments or stdin protocol; exit 2.
    Usage(String),
    /// A refusal or I/O failure other than ownership; exit 1.
    Operation(String),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Operation(format!("I/O failed: {error}"))
    }
}

fn refused(action: &str, refusal: &LogFileRefusal) -> Failure {
    Failure::Operation(format!("{action} refused: {refusal:?}"))
}

fn print_line(value: &Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

/// Print one line already spelled as JSON.
fn print_text(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn absolute_root(text: &str) -> Result<&Path, Failure> {
    let root = Path::new(text);
    if !root.is_absolute() {
        return Err(Failure::Usage(format!("root must be absolute: {text:?}")));
    }
    Ok(root)
}

/// A nonempty id argument, spelled as `bake_session` holds a Session id.
fn nonempty_id(text: &str) -> Result<String, Failure> {
    if text.is_empty() {
        return Err(Failure::Usage("id must not be empty".to_owned()));
    }
    Ok(bake_session::js_string::from_rust(text).into_owned())
}

/// A decimal safe integer with no sign, as the `seq` argument.
fn safe_seq(text: &str) -> Result<u64, Failure> {
    let usage = || Failure::Usage(format!("seq must be a decimal safe integer: {text:?}"));
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(usage());
    }
    match text.parse::<u64>() {
        Ok(seq) if seq <= MAX_SAFE_INTEGER => Ok(seq),
        _ => Err(usage()),
    }
}

/// A decimal byte count of at least 1, as the tear budget.
fn tear_budget(text: &str) -> Result<u64, Failure> {
    match safe_seq(text) {
        Ok(bytes) if bytes > 0 => Ok(bytes),
        _ => Err(Failure::Usage(format!(
            "bytes must be a positive decimal safe integer: {text:?}"
        ))),
    }
}

/// Where `pause-migrate` pauses a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pause {
    /// Before the migration's temporary file is created.
    Publish,
    /// After the temporary file was linked into place, before its removal.
    Unlink,
}

/// The real filesystem, except that the writes stop for good once they
/// reach the tear budget, or the migration pauses once: see the module
/// comment.
#[derive(Debug)]
struct ProbeIo {
    remaining: Option<AtomicU64>,
    pause: Option<Pause>,
    paused: AtomicBool,
}

/// Whether `path` names a migration's temporary file.
fn migration_temporary(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("session.migration."))
}

impl ProbeIo {
    /// Store `bytes` up to the budget; past it, report the tear and wait for
    /// the kill, or exit at stdin EOF, without returning.
    fn write_within_budget(
        remaining: &AtomicU64,
        file: &mut File,
        offset: u64,
        bytes: &[u8],
    ) -> io::Result<()> {
        let left = remaining.load(Ordering::SeqCst);
        // A byte count of an in-memory buffer fits in u64.
        let length = bytes.len() as u64;
        if length < left {
            remaining.store(left - length, Ordering::SeqCst);
            return RealIo.write_at(file, offset, bytes);
        }
        // `left` is below `bytes.len()`, so it fits in usize.
        let kept = usize::try_from(left).unwrap_or(bytes.len());
        RealIo.write_at(file, offset, &bytes[..kept])?;
        print_line(&json!({ "state": "torn" }))?;
        let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
        std::process::exit(0)
    }

    /// Pause once at `at` when the probe pauses there: print `paused`, then
    /// resume on the stdin line `go`, exit 0 at EOF, or exit 2 on anything
    /// else.
    fn pause_at(&self, at: Pause) -> io::Result<()> {
        if self.pause != Some(at) || self.paused.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        print_line(&json!({ "state": "paused" }))?;
        let mut line = Vec::new();
        io::stdin()
            .lock()
            .take(MAX_COMMAND_BYTES)
            .read_until(b'\n', &mut line)?;
        match line.as_slice() {
            b"go\n" => Ok(()),
            b"" => std::process::exit(0),
            other => {
                eprintln!(
                    "bake-session-lease-probe: unexpected pause command {:?}",
                    String::from_utf8_lossy(other)
                );
                std::process::exit(2)
            }
        }
    }
}

impl StorageIo for ProbeIo {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        RealIo.create_dir_all(dir)
    }
    fn create_new(&self, path: &Path) -> io::Result<File> {
        if migration_temporary(path) {
            self.pause_at(Pause::Publish)?;
        }
        RealIo.create_new(path)
    }
    fn open_write(&self, path: &Path) -> io::Result<File> {
        RealIo.open_write(path)
    }
    fn write_at(&self, file: &mut File, offset: u64, bytes: &[u8]) -> io::Result<()> {
        match &self.remaining {
            Some(remaining) => Self::write_within_budget(remaining, file, offset, bytes),
            None => RealIo.write_at(file, offset, bytes),
        }
    }
    fn set_len(&self, file: &File, len: u64) -> io::Result<()> {
        RealIo.set_len(file, len)
    }
    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        RealIo.hard_link(original, link)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        if migration_temporary(path) {
            self.pause_at(Pause::Unlink)?;
        }
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
    fn stat_dir(&self, path: &Path) -> io::Result<Option<bool>> {
        RealIo.stat_dir(path)
    }
    fn open_lock(&self, path: &Path) -> io::Result<File> {
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
    fn stat(&self, path: &Path, follow: bool) -> io::Result<FileStat> {
        RealIo.stat(path, follow)
    }
}

fn tear_io(bytes: u64) -> Arc<ProbeIo> {
    Arc::new(ProbeIo {
        remaining: Some(AtomicU64::new(bytes)),
        pause: None,
        paused: AtomicBool::new(false),
    })
}

fn first_turn() -> [Value; 2] {
    [
        json!({ "type": "turn/start", "seq": 0, "time": 1, "data": { "turn": 1 } }),
        json!({
            "type": "turn/end",
            "seq": 1,
            "time": 2,
            "data": { "turn": 1, "reason": { "kind": "completed" } }
        }),
    ]
}

fn untorn() -> Failure {
    Failure::Usage("the writes ended within the tear budget".to_owned())
}

fn run_tear_create(root: &Path, id: &str, bytes: u64) -> Result<ExitCode, Failure> {
    let header = json!({ "version": 3, "id": id, "createdAt": 1000, "isSeeded": false });
    let mut handle = PlainLogFile::create_with_io(tear_io(bytes), root, &header, None)
        .map_err(|error| refused("create", &error))?;
    handle
        .append(&first_turn())
        .map_err(|error| refused("append", &error))?;
    Err(untorn())
}

fn run_tear_append(root: &Path, id: &str, seq: u64, bytes: u64) -> Result<ExitCode, Failure> {
    let mut handle = PlainLogFile::open_with_io(tear_io(bytes), root, id, SOURCE_BUDGET)
        .map_err(|error| refused("open", &error))?;
    let events = [
        json!({ "type": "turn/start", "seq": seq, "time": 3, "data": { "turn": 2 } }),
        json!({
            "type": "turn/end",
            "seq": seq + 1,
            "time": 4,
            "data": { "turn": 2, "reason": { "kind": "completed" } }
        }),
    ];
    handle
        .append(&events)
        .map_err(|error| refused("append", &error))?;
    Err(untorn())
}

/// Print readiness, then keep `handle` until stdin says `release` or ends.
fn hold(handle: PlainLogFile) -> Result<(), Failure> {
    print_line(&json!({ "state": "holding" }))?;
    let mut stdin = io::stdin().lock();
    let mut line = Vec::new();
    stdin
        .by_ref()
        .take(MAX_COMMAND_BYTES)
        .read_until(b'\n', &mut line)?;
    match line.as_slice() {
        b"" => {
            drop(handle);
            Ok(())
        }
        b"release\n" => {
            drop(handle);
            print_line(&json!({ "state": "released" }))?;
            Ok(())
        }
        other => Err(Failure::Usage(format!(
            "unexpected holder command {:?}",
            String::from_utf8_lossy(other)
        ))),
    }
}

fn run_hold(root: &Path, id: &str) -> Result<ExitCode, Failure> {
    let header = json!({ "version": 3, "id": id, "createdAt": 1000, "isSeeded": false });
    let mut handle =
        PlainLogFile::create(root, &header, None).map_err(|error| refused("create", &error))?;
    handle
        .append(&first_turn())
        .map_err(|error| refused("append", &error))?;
    hold(handle)?;
    Ok(ExitCode::SUCCESS)
}

fn run_hold_open(root: &Path, id: &str) -> Result<ExitCode, Failure> {
    let handle =
        PlainLogFile::open(root, id, SOURCE_BUDGET).map_err(|error| refused("open", &error))?;
    hold(handle)?;
    Ok(ExitCode::SUCCESS)
}

fn run_open(root: &Path, id: &str, seq: u64) -> Result<ExitCode, Failure> {
    report_open(PlainLogFile::open(root, id, SOURCE_BUDGET), seq)
}

fn run_pause_migrate(root: &Path, id: &str, pause: Pause, seq: u64) -> Result<ExitCode, Failure> {
    let io = Arc::new(ProbeIo {
        remaining: None,
        pause: Some(pause),
        paused: AtomicBool::new(false),
    });
    let opened = PlainLogFile::open_with_io(io.clone(), root, id, SOURCE_BUDGET);
    if !io.paused.load(Ordering::SeqCst) {
        return Err(Failure::Usage(
            "the open reached no migration pause".to_owned(),
        ));
    }
    report_open(opened, seq)
}

/// Report a write `open`: append `turn/start` at `seq`, close, and print the
/// stored seqs, or print the refusal; see the module comment.
fn report_open(
    opened: Result<PlainLogFile, LogFileRefusal>,
    seq: u64,
) -> Result<ExitCode, Failure> {
    let mut handle = match opened {
        Ok(handle) => handle,
        Err(LogFileRefusal::AlreadyOwned { message }) => {
            print_line(&json!({ "outcome": "owned", "message": message }))?;
            return Ok(ExitCode::from(OWNED_EXIT));
        }
        Err(LogFileRefusal::Io(error)) => {
            print_line(&json!({ "outcome": "io", "kind": format!("{:?}", error.kind()) }))?;
            return Ok(ExitCode::FAILURE);
        }
        Err(error) => {
            let Some(message) = error.message() else {
                return Err(refused("open", &error));
            };
            print_text(&json_text(
                &json!({ "outcome": "refused", "message": message }),
            ))?;
            return Ok(ExitCode::FAILURE);
        }
    };
    let event = json!({ "type": "turn/start", "seq": seq, "time": 3, "data": { "turn": 2 } });
    handle
        .append(&[event])
        .map_err(|error| refused("append", &error))?;
    let path = handle.path().to_owned();
    drop(handle);
    // Report what the file holds, not what was asked for.
    let bytes = std::fs::read(&path)?;
    let scanned = scan_log(&bytes, PathPlatform::host(), SOURCE_BUDGET).map_err(|error| {
        Failure::Operation(format!("scan of the written log failed: {error:?}"))
    })?;
    let seqs = scanned
        .rows()
        .iter()
        .map(|row| row.get("seq").and_then(Value::as_u64))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Failure::Operation("a stored row has no integer seq".to_owned()))?;
    if seqs.last() != Some(&seq) {
        return Err(Failure::Operation(format!(
            "the written log ends at {:?}, not seq {seq}",
            seqs.last()
        )));
    }
    print_line(&json!({ "outcome": "opened", "seqs": seqs }))?;
    Ok(ExitCode::SUCCESS)
}

fn run() -> Result<ExitCode, Failure> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["hold", root, id] => run_hold(absolute_root(root)?, &nonempty_id(id)?),
        ["hold-open", root, id] => run_hold_open(absolute_root(root)?, &nonempty_id(id)?),
        ["open", root, id, seq] => {
            run_open(absolute_root(root)?, &nonempty_id(id)?, safe_seq(seq)?)
        }
        ["pause-migrate", root, id, pause, seq] => {
            let pause = match *pause {
                "publish" => Pause::Publish,
                "unlink" => Pause::Unlink,
                other => {
                    return Err(Failure::Usage(format!(
                        "pause must be publish or unlink: {other:?}"
                    )));
                }
            };
            run_pause_migrate(absolute_root(root)?, &nonempty_id(id)?, pause, safe_seq(seq)?)
        }
        ["tear-create", root, id, bytes] => {
            run_tear_create(absolute_root(root)?, &nonempty_id(id)?, tear_budget(bytes)?)
        }
        ["tear-append", root, id, seq, bytes] => {
            let seq = safe_seq(seq)?;
            if seq >= MAX_SAFE_INTEGER {
                return Err(Failure::Usage(format!("seq {seq} has no next safe seq")));
            }
            run_tear_append(
                absolute_root(root)?,
                &nonempty_id(id)?,
                seq,
                tear_budget(bytes)?,
            )
        }
        _ => Err(Failure::Usage(
            "expected hold|hold-open <absolute-root> <id>, open <absolute-root> <id> <seq>, \
             pause-migrate <absolute-root> <id> <publish|unlink> <seq>, tear-create <absolute-root> <id> <bytes>, or \
             tear-append <absolute-root> <id> <seq> <bytes>"
                .to_owned(),
        )),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(Failure::Usage(message)) => {
            eprintln!("bake-session-lease-probe: {message}");
            ExitCode::from(2)
        }
        Err(Failure::Operation(message)) => {
            eprintln!("bake-session-lease-probe: {message}");
            ExitCode::FAILURE
        }
    }
}
