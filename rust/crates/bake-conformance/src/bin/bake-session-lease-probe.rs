//! Development-only Session write-lease probe for cross-runtime tests, not an
//! agent. It drives one public `bake_session::PlainLogFile` handle per process
//! so tests can contend for a Session's write lock against another process.
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
//!   prints `{"outcome":"opened","seqs":[...]}` with every stored seq.
//!
//! A holder releases on the exact stdin line `release`, printing
//! `{"state":"released"}` after the handle is dropped, and on stdin EOF
//! silently; both exit 0. Any other stdin input is a fixture error. No
//! signal handler is installed, so a killed holder never runs `Drop`.
//!
//! Exit codes: 0 success, 1 any refusal or I/O failure other than ownership,
//! 2 usage or fixture error, 3 owned. Failures write only to stderr.

use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use bake_session::{LogFileRefusal, PathPlatform, PlainLogFile, scan_log};
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
    let events = [
        json!({ "type": "turn/start", "seq": 0, "time": 1, "data": { "turn": 1 } }),
        json!({
            "type": "turn/end",
            "seq": 1,
            "time": 2,
            "data": { "turn": 1, "reason": { "kind": "completed" } }
        }),
    ];
    handle
        .append(&events)
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
    let mut handle = match PlainLogFile::open(root, id, SOURCE_BUDGET) {
        Ok(handle) => handle,
        Err(LogFileRefusal::AlreadyOwned { message }) => {
            print_line(&json!({ "outcome": "owned", "message": message }))?;
            return Ok(ExitCode::from(OWNED_EXIT));
        }
        Err(error) => return Err(refused("open", &error)),
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
        _ => Err(Failure::Usage(
            "expected hold|hold-open <absolute-root> <id>, or open <absolute-root> <id> <seq>"
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
