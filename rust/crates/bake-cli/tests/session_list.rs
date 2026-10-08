//! Runs the built `bake-rs session list` against every case of
//! `conformance/session/list-cases.json` that applies to this host. The
//! table's expectations were written from the TypeScript sources, and its
//! TypeScript spec checks the same layouts through the production backend's
//! `list`; a `rust` entry replaces the expectation only where this preview
//! reports a native limit. The backend promises no order, so table cases
//! compare sessions sorted by path; a separate test pins the native order.
//!
//! Each run owns a private scratch directory, created exclusively under the
//! system temporary directory, that holds the case layout, run as the
//! child's working directory, and an empty home that the child sees as
//! `HOME`, `BAKE_HOME`, `DSH_HOME`, and `USERPROFILE`. Every entry under the
//! layout, links and modification times included, must be unchanged after
//! the run, the home must stay empty, and nothing else may appear in the
//! scratch directory. Each child is waited on with a deadline and killed and
//! reaped if the deadline is exceeded, and both output streams are drained to the end.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Process creation dominates; this bounds a hung child, never a result.
const DEADLINE: Duration = Duration::from_secs(60);
const CASE_COUNT: usize = 58;
const POSIX_ONLY: usize = 11;

/// A private directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    /// Create a new directory exclusively, so no other test, process, or
    /// job can share it; an existing name is never reused.
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let temp = std::env::temp_dir();
        for _ in 0..64 {
            let count = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let path = temp.join(format!(
                "bake-cli-list-{}-{count}-{nanos:08x}",
                std::process::id()
            ));
            match private_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create scratch {path:?}: {error}"),
            }
        }
        panic!("no unused scratch name under {temp:?}");
    }

    fn case(&self) -> PathBuf {
        self.0.join("case")
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    /// Create the empty layout and home directories.
    fn prepare(&self) {
        std::fs::create_dir(self.case()).unwrap();
        std::fs::create_dir(self.home()).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

struct Run {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Wait for the child, draining its piped streams. At the deadline, or when
/// polling fails, the child is killed and reaped and both drains are joined
/// before the test fails, so no child or thread outlives the call.
fn finish(mut child: Child) -> Run {
    fn drain<R: Read + Send + 'static>(
        stream: Option<R>,
    ) -> std::thread::JoinHandle<io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                stream.read_to_end(&mut bytes)?;
            }
            Ok(bytes)
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let polled = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() <= DEADLINE => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => break Err(format!("bake-rs did not exit within {DEADLINE:?}")),
            Err(error) => break Err(format!("polling bake-rs failed: {error}")),
        }
    };
    let status = polled.map_err(|failure| {
        // A child that exited after the last poll makes kill fail; wait still reaps it.
        let killed = child.kill();
        let reaped = child.wait();
        format!("{failure} (kill: {killed:?}, wait: {reaped:?})")
    });
    let (stdout, stderr) = (stdout.join(), stderr.join());
    let status = status.unwrap_or_else(|failure| panic!("{failure}"));
    Run {
        status,
        stdout: stdout.expect("stdout drain").expect("read stdout"),
        stderr: stderr.expect("stderr drain").expect("read stderr"),
    }
}

/// Run `session list` in `scratch`'s layout with `args` after the
/// subcommand, and check that the layout, the empty home, and the scratch
/// directory itself are unchanged.
fn list(scratch: &Scratch, args: &[&OsStr]) -> Run {
    let (dir, home) = (scratch.case(), scratch.home());
    let before = tree(&dir);
    let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
    command
        .args(["session", "list"])
        .args(args)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in ["HOME", "BAKE_HOME", "DSH_HOME", "USERPROFILE"] {
        command.env(name, &home);
    }
    let run = finish(command.spawn().expect("spawn bake-rs"));
    assert_eq!(tree(&dir), before, "the layout changed");
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0, "home written");
    let mut names: Vec<_> = std::fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["case", "home"], "the scratch directory changed");
    run
}

/// `session list` with the usual options, built as `OsStr` arguments.
fn list_args(
    root: &str,
    max_header_bytes: u64,
    max_entries: u64,
    compression: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--root".to_owned(),
        root.to_owned(),
        "--max-header-bytes".to_owned(),
        max_header_bytes.to_string(),
        "--max-entries".to_owned(),
        max_entries.to_string(),
    ];
    if let Some(compression) = compression {
        args.extend(["--compression".to_owned(), compression.to_owned()]);
    }
    args
}

fn run_list(scratch: &Scratch, args: &[String]) -> Run {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    list(scratch, &args)
}

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex {text:?}");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// One Zstandard frame of raw blocks: no content size or checksum, a 128 KiB
/// window, and blocks of at most 128 KiB.
fn raw_frame(content: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38];
    let mut blocks = content.chunks(131_072).peekable();
    if blocks.peek().is_none() {
        frame.extend([1, 0, 0]);
    }
    while let Some(block) = blocks.next() {
        let header =
            (u32::try_from(block.len()).unwrap() << 3) | u32::from(blocks.peek().is_none());
        frame.extend(&header.to_le_bytes()[..3]);
        frame.extend(block);
    }
    frame
}

/// Build a table layout: `dir`, `symlink` with `target`, or `file` with
/// `text` or with `frames` and an optional `appendHex`.
fn build(dir: &Path, layout: &Value) {
    for entry in layout.as_array().unwrap() {
        let at = |key: &str| dir.join(entry[key].as_str().unwrap());
        if entry.get("dir").is_some() {
            std::fs::create_dir_all(at("dir")).unwrap();
        } else if entry.get("symlink").is_some() {
            let link = at("symlink");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(entry["target"].as_str().unwrap(), &link);
        } else {
            let path = at("file");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = if let Some(text) = entry.get("text") {
                text.as_str().unwrap().as_bytes().to_vec()
            } else {
                let mut bytes: Vec<u8> = entry["frames"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|frame| raw_frame(frame.as_str().unwrap().as_bytes()))
                    .collect();
                bytes.extend(hex(entry
                    .get("appendHex")
                    .and_then(Value::as_str)
                    .unwrap_or("")));
                bytes
            };
            std::fs::write(&path, bytes).unwrap();
        }
    }
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(not(unix))]
fn symlink(_: &str, _: &Path) {
    unreachable!("symlink cases run only on POSIX hosts");
}

type Tree = Vec<(PathBuf, &'static str, Vec<u8>, Option<SystemTime>)>;

/// Every entry under `dir`, not following links: kind, bytes or link
/// target, and a file's or directory's modification time.
fn snapshot(dir: &Path, prefix: &Path, out: &mut Tree) {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let relative = prefix.join(&name);
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        if metadata.is_symlink() {
            let target = std::fs::read_link(&path).unwrap();
            out.push((
                relative,
                "link",
                target.into_os_string().into_encoded_bytes(),
                None,
            ));
        } else if metadata.is_dir() {
            out.push((
                relative.clone(),
                "dir",
                Vec::new(),
                metadata.modified().ok(),
            ));
            snapshot(&path, &relative, out);
        } else if metadata.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            out.push((relative, "file", bytes, metadata.modified().ok()));
        } else {
            // A FIFO or other special file: never opened, which could block.
            out.push((relative, "special", Vec::new(), metadata.modified().ok()));
        }
    }
}

fn tree(dir: &Path) -> Tree {
    let mut out = Vec::new();
    snapshot(dir, Path::new(""), &mut out);
    out
}

/// Only `posix` limits a case in this table, and it names its reason.
fn runs_here(case: &Value) -> bool {
    case.get("platforms").is_none_or(|platforms| {
        assert_eq!(platforms, &json!(["posix"]), "{}", case["id"]);
        assert!(
            case["platformReason"]
                .as_str()
                .is_some_and(|r| !r.is_empty()),
            "{}: a platform limit needs its reason",
            case["id"]
        );
        cfg!(not(windows))
    })
}

fn budget(table: &Value, case: &Value, key: &str) -> u64 {
    case.get(key)
        .unwrap_or(&table["defaults"][key])
        .as_u64()
        .unwrap()
}

/// The exact keys of an object.
fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("an object: {value}"))
        .keys()
        .map(String::as_str)
        .collect()
}

/// Parse a run's single stdout record and check the parts every listed or
/// refused record shares: exit status 0 or 3 to match, empty stderr, and the
/// exact top-level shape.
fn record(id: &str, run: &Run, encoding: &str) -> Value {
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.stderr.is_empty(), "{id}: {stderr}");
    let text = std::str::from_utf8(&run.stdout).unwrap();
    let line = text
        .strip_suffix('\n')
        .unwrap_or_else(|| panic!("{id}: one record, {text:?}"));
    assert!(!line.contains('\n'), "{id}: {text}");
    let record: Value = serde_json::from_str(line).unwrap();
    assert_eq!(record["schema"], "bake/session-list", "{id}");
    assert_eq!(record["version"], 1, "{id}");
    assert_eq!(record["encoding"], encoding, "{id}");
    let sessions = record["sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("{id}: sessions array in {record}"));
    let mut expected_keys = BTreeSet::from(["schema", "version", "status", "encoding", "sessions"]);
    match record["status"].as_str() {
        Some("listed") => {
            assert_eq!(run.status.code(), Some(0), "{id}: {record}");
            for session in sessions {
                assert_eq!(
                    keys(session),
                    BTreeSet::from(["path", "storedVersion", "header", "sizeBytes"]),
                    "{id}: {session}"
                );
            }
        }
        Some("refused") => {
            assert_eq!(run.status.code(), Some(3), "{id}: {record}");
            // A refusal never carries a partial result.
            assert!(sessions.is_empty(), "{id}: {record}");
            expected_keys.insert("refusal");
            let refusal = &record["refusal"];
            for key in ["stage", "reason", "kind", "message", "path"] {
                assert!(refusal.get(key).is_some(), "{id}: {key} of {record}");
            }
            assert!(
                refusal["message"].as_str().is_some_and(|m| !m.is_empty()),
                "{id}"
            );
        }
        _ => panic!("{id}: unknown status in {record}"),
    }
    assert_eq!(keys(&record), expected_keys, "{id}: {record}");
    record
}

/// A refused record's stage, reason, kind, and path.
fn assert_refused(id: &str, record: &Value, stage: &str, reason: &str, kind: &str, path: Value) {
    assert_eq!(record["status"], "refused", "{id}: {record}");
    let refusal = &record["refusal"];
    assert_eq!(refusal["stage"], stage, "{id}: {record}");
    assert_eq!(refusal["reason"], reason, "{id}: {record}");
    assert_eq!(refusal["kind"], kind, "{id}: {record}");
    assert_eq!(refusal["path"], path, "{id}: {record}");
}

/// A failure: exit status 1, no stdout, and one `bake-rs: ` diagnostic line.
fn failure_line(id: &str, run: &Run) -> String {
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(1), "{id}: {stderr}");
    assert!(
        run.stdout.is_empty(),
        "{id}: {}",
        String::from_utf8_lossy(&run.stdout)
    );
    let line = stderr
        .strip_prefix("bake-rs: ")
        .and_then(|line| line.strip_suffix('\n'))
        .unwrap_or_else(|| panic!("{id}: one diagnostic line, {stderr}"));
    assert!(!line.contains('\n'), "{id}: {stderr}");
    line.to_owned()
}

/// Check one run against an expectation, which names only the fields the
/// shared table compares across both runtimes.
fn check(id: &str, dir: &Path, case: &Value, run: &Run, expected: &Value) {
    let outcome = expected["outcome"].as_str().unwrap();
    if outcome == "failure" {
        let line = failure_line(id, run);
        let diagnostic = &case["nativeDiagnostic"];
        let text = diagnostic["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{id}: a failure case needs nativeDiagnostic"));
        assert!(line.contains(text), "{id}: {line}");
        let relative: PathBuf = diagnostic["path"].as_str().unwrap().split('/').collect();
        let quoted = format!("{relative:?}");
        let tail = format!("{}\"", &quoted[1..quoted.len() - 1]);
        assert!(line.contains(&tail), "{id}: {line} lacks {tail}");
        return;
    }
    let encoding = case
        .get("compression")
        .and_then(Value::as_str)
        .unwrap_or("zstd");
    let record = record(id, run, encoding);
    assert_eq!(record["status"], outcome, "{id}: {record}");
    if outcome == "refused" {
        let refusal = &record["refusal"];
        for key in ["stage", "reason", "kind", "path"] {
            assert_eq!(refusal[key], expected[key], "{id}: {key} of {record}");
        }
        if let Some(message) = expected["message"].as_str() {
            let actual = refusal["message"].as_str().unwrap();
            assert!(actual.contains(message), "{id}: {actual:?}");
        }
        return;
    }
    assert_eq!(outcome, "listed", "{id}");
    let root = dir.join(case["root"].as_str().unwrap());
    let mut actual = record["sessions"].as_array().unwrap().clone();
    let by_path = |session: &Value| session["path"].as_str().unwrap().to_owned();
    actual.sort_by_key(by_path);
    let mut wanted = expected["sessions"].as_array().unwrap().clone();
    wanted.sort_by_key(by_path);
    assert_eq!(actual.len(), wanted.len(), "{id}: {record}");
    for (session, want) in actual.iter().zip(&wanted) {
        for key in ["path", "storedVersion", "header"] {
            assert_eq!(session[key], want[key], "{id}: {key} of {session}");
        }
        // `sizeBytes` is the listed file's size, following a link.
        let path = root.join(session["path"].as_str().unwrap());
        let size = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("{id}: {path:?}: {error}"))
            .len();
        assert_eq!(session["sizeBytes"], size, "{id}: {session}");
    }
}

#[test]
fn lists_match_the_shared_table_and_leave_the_root_unchanged() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../conformance/session/list-cases.json");
    let table: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(table["schema"], "bake/session-conformance/list-cases");
    assert_eq!(table["version"], 1);
    let cases = table["cases"].as_array().unwrap();
    assert_eq!(cases.len(), CASE_COUNT);
    let mut ids = BTreeSet::new();
    let mut posix_only = 0;
    let mut ran = 0;
    for case in cases {
        let id = case["id"].as_str().unwrap();
        assert!(ids.insert(id), "{id}: a repeated case id");
        if case.get("platforms").is_some() {
            posix_only += 1;
        }
        if !runs_here(case) {
            continue;
        }
        let scratch = Scratch::new();
        scratch.prepare();
        build(&scratch.case(), &case["layout"]);
        let args = list_args(
            case["root"].as_str().unwrap(),
            budget(&table, case, "maxHeaderBytes"),
            budget(&table, case, "maxEntries"),
            case.get("compression").and_then(Value::as_str),
        );
        let run = run_list(&scratch, &args);
        let expected = case.get("rust").unwrap_or(&case["ts"]);
        check(id, &scratch.case(), case, &run, expected);
        ran += 1;
    }
    assert_eq!(posix_only, POSIX_ONLY);
    let skipped = if cfg!(windows) { posix_only } else { 0 };
    assert_eq!(ran, cases.len() - skipped);
}

/// A current plain header line for a Session without a working directory.
fn plain_header(id: &str, created_at: u64) -> String {
    format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"createdAt\":{created_at},\"isSeeded\":false,\"delegationDepth\":0}}\n"
    )
}

/// Write a file under the scratch layout, creating its directories.
fn write(scratch: &Scratch, relative: &str, bytes: &[u8]) {
    let path = scratch.case().join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Sessions come out in byte order of the project and then the Session
/// directory names, which is not the order of their paths: `a` precedes
/// `a-b` as a name, but `a/` sorts after `a-` in a path. Repeated runs give
/// the same order.
#[test]
fn sessions_follow_the_byte_order_of_the_traversal() {
    let scratch = Scratch::new();
    scratch.prepare();
    write(
        &scratch,
        "root/_no-cwd/a-b/session.v3.jsonl",
        plain_header("a-b", 2).as_bytes(),
    );
    write(
        &scratch,
        "root/_no-cwd/a/session.v3.jsonl",
        plain_header("a", 1).as_bytes(),
    );
    let args = list_args("root", 4096, 64, Some("none"));
    for _ in 0..2 {
        let record = record("order", &run_list(&scratch, &args), "none");
        let paths: Vec<_> = record["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["path"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            paths,
            ["_no-cwd/a/session.v3.jsonl", "_no-cwd/a-b/session.v3.jsonl"],
            "{record}"
        );
    }
}

/// An absent root lists nothing and is not created.
#[test]
fn an_absent_root_lists_nothing_and_stays_absent() {
    let scratch = Scratch::new();
    scratch.prepare();
    let record = record(
        "absent-root",
        &run_list(&scratch, &list_args("root", 4096, 1, None)),
        "zstd",
    );
    assert_eq!(record["status"], "listed");
    assert_eq!(record["sessions"], json!([]));
    assert!(!scratch.case().join("root").exists());
}

/// `--max-entries` spans the layout pass and the discovery pass, each of
/// which lists the root, the project, and the Session directory once here;
/// the refusal names the pass that ran out.
#[test]
fn the_entry_budget_spans_both_passes() {
    let layout = |scratch: &Scratch| {
        write(
            scratch,
            "root/_no-cwd/a1/session.v3.jsonl",
            plain_header("a1", 1).as_bytes(),
        );
    };
    let scratch = Scratch::new();
    scratch.prepare();
    layout(&scratch);
    let listed = record(
        "exact",
        &run_list(&scratch, &list_args("root", 4096, 6, Some("none"))),
        "none",
    );
    assert_eq!(listed["status"], "listed", "{listed}");
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
    for (max_entries, stage) in [(5, "discovery"), (3, "discovery"), (2, "layout")] {
        let id = format!("entries-{max_entries}");
        let run = run_list(
            &scratch,
            &list_args("root", 4096, max_entries, Some("none")),
        );
        let record = record(&id, &run, "none");
        assert_refused(
            &id,
            &record,
            stage,
            "entry-budget",
            "native-limit",
            Value::Null,
        );
    }
}

/// A header past `--max-header-bytes` aborts the whole listing rather than
/// omitting that Session, so an admitted Session before it is not reported.
#[test]
fn a_header_past_its_budget_refuses_the_whole_listing() {
    let scratch = Scratch::new();
    scratch.prepare();
    let small = plain_header("a0", 1);
    let large = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"a1\",\"createdAt\":2,\"isSeeded\":false,\"delegationDepth\":0,\"agentPreset\":\"{}\"}}\n",
        "p".repeat(512)
    );
    write(
        &scratch,
        "root/_no-cwd/a0/session.v3.jsonl",
        small.as_bytes(),
    );
    write(
        &scratch,
        "root/_no-cwd/a1/session.v3.jsonl",
        large.as_bytes(),
    );
    let budget = u64::try_from(large.len()).unwrap();
    let listed = record(
        "fits",
        &run_list(&scratch, &list_args("root", budget, 64, Some("none"))),
        "none",
    );
    assert_eq!(listed["status"], "listed", "{listed}");
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 2);
    let run = run_list(&scratch, &list_args("root", budget - 1, 64, Some("none")));
    let refused = record("past", &run, "none");
    assert_refused(
        "past",
        &refused,
        "header",
        "header-budget",
        "native-limit",
        json!("_no-cwd/a1/session.v3.jsonl"),
    );
}

/// A FIFO named as a generation fails as an irregular file without waiting
/// for a writer, which TypeScript's blocking open would do, and the Session
/// admitted before it is not printed.
#[cfg(unix)]
#[test]
fn a_fifo_generation_fails_without_blocking() {
    let scratch = Scratch::new();
    scratch.prepare();
    write(
        &scratch,
        "root/_no-cwd/a0/session.v3.jsonl",
        plain_header("a0", 1).as_bytes(),
    );
    let dir = scratch.case().join("root/_no-cwd/a1");
    std::fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join("session.v3.jsonl");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `path` is a valid NUL-terminated string for the call's duration.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let run = run_list(&scratch, &list_args("root", 4096, 64, Some("none")));
    let line = failure_line("fifo", &run);
    assert!(line.contains("is not a regular file"), "{line}");
    assert!(line.contains("session.v3.jsonl\""), "{line}");
}

/// A Session directory name that is not UTF-8, which Node would read with
/// replacement characters, is a native limit of the layout pass. Linux file
/// systems store such names; macOS and Windows reject them.
#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_session_directory_is_a_native_limit() {
    use std::os::unix::ffi::OsStrExt;
    let scratch = Scratch::new();
    scratch.prepare();
    write(
        &scratch,
        "root/_no-cwd/a0/session.v3.jsonl",
        plain_header("a0", 1).as_bytes(),
    );
    let project = scratch.case().join("root/_no-cwd");
    std::fs::create_dir(project.join(OsStr::from_bytes(b"a\xff"))).unwrap();
    let run = run_list(&scratch, &list_args("root", 4096, 64, Some("none")));
    let record = record("non-utf8", &run, "none");
    assert_refused(
        "non-utf8",
        &record,
        "layout",
        "non-utf8-name",
        "native-limit",
        Value::Null,
    );
}
