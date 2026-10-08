//! Runs the built `bake-rs session stat` against every case of
//! `conformance/session/stat-cases.json` that applies to this host. The
//! table's expectations were written from the TypeScript sources before
//! either harness ran, and its TypeScript spec checks the same layouts
//! through the production backend's `stat`; a `rust` entry replaces the
//! expectation only where this preview reports a native budget.
//!
//! Each case owns a fresh directory, run as the child's working directory,
//! and an empty home directory that the child sees as `HOME`, `BAKE_HOME`,
//! `DSH_HOME`, and `USERPROFILE`. Every entry under the case directory,
//! links included, must be unchanged after the run, and the home must stay
//! empty. Each child is waited on with a deadline and killed and reaped
//! when it passes.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// Process creation dominates; this bounds a hung child, never a result.
const DEADLINE: Duration = Duration::from_secs(60);
const CASE_COUNT: usize = 46;
const POSIX_ONLY: usize = 5;

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bake-cli-stat-{}-{count}", std::process::id()));
        std::fs::create_dir(&path).expect("create an unused scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    ) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
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

fn hex(text: &str) -> Vec<u8> {
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
/// target, and a file's modification time.
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
            out.push((relative.clone(), "dir", Vec::new(), None));
            snapshot(&path, &relative, out);
        } else {
            let bytes = std::fs::read(&path).unwrap();
            out.push((relative, "file", bytes, metadata.modified().ok()));
        }
    }
}

fn tree(dir: &Path) -> Tree {
    let mut out = Vec::new();
    snapshot(dir, Path::new(""), &mut out);
    out
}

/// Only `posix` limits a case in this table.
fn runs_here(case: &Value) -> bool {
    case.get("platforms").is_none_or(|platforms| {
        assert_eq!(platforms, &serde_json::json!(["posix"]));
        cfg!(not(windows))
    })
}

fn budget(table: &Value, case: &Value, key: &str) -> String {
    case.get(key)
        .unwrap_or(&table["defaults"][key])
        .as_u64()
        .unwrap()
        .to_string()
}

/// Check one record against an expectation, which names only the fields the
/// shared table compares across both runtimes.
fn check(id: &str, dir: &Path, case: &Value, run: &Run, expected: &Value) {
    let stderr = String::from_utf8_lossy(&run.stderr);
    let code = run.status.code();
    if expected["outcome"] == "failure" {
        assert_eq!(code, Some(1), "{id}: {stderr}");
        assert!(run.stdout.is_empty(), "{id}");
        let diagnostic = &case["nativeDiagnostic"];
        let line = stderr
            .strip_prefix("bake-rs: ")
            .and_then(|line| line.strip_suffix('\n'))
            .unwrap_or_else(|| panic!("{id}: one diagnostic line, {stderr}"));
        assert!(!line.contains('\n'), "{id}: {stderr}");
        assert!(
            line.contains(diagnostic["text"].as_str().unwrap()),
            "{id}: {stderr}"
        );
        let relative: PathBuf = diagnostic["path"].as_str().unwrap().split('/').collect();
        let quoted = format!("{relative:?}");
        let tail = format!("{}\"", &quoted[1..quoted.len() - 1]);
        assert!(line.contains(&tail), "{id}: {stderr} lacks {tail}");
        return;
    }
    assert!(stderr.is_empty(), "{id}: {stderr}");
    let text = std::str::from_utf8(&run.stdout).unwrap();
    let line = text
        .strip_suffix('\n')
        .unwrap_or_else(|| panic!("{id}: one record, {stderr}"));
    assert!(!line.contains('\n'), "{id}");
    let record: Value = serde_json::from_str(line).unwrap();
    assert_eq!(record["schema"], "bake/session-stat", "{id}");
    assert_eq!(record["version"], 1, "{id}");
    let encoding = case
        .get("compression")
        .and_then(Value::as_str)
        .unwrap_or("zstd");
    assert_eq!(record["encoding"], encoding, "{id}");
    let outcome = expected["outcome"].as_str().unwrap();
    let status = if outcome == "refused" { 3 } else { 0 };
    assert_eq!(code, Some(status), "{id}: {record}");
    assert_eq!(record["status"], outcome, "{id}");
    if outcome == "refused" {
        let refusal = &record["refusal"];
        for key in ["stage", "reason", "kind", "path"] {
            assert_eq!(refusal[key], expected[key], "{id}: {key} of {record}");
        }
        if let Some(message) = expected["message"].as_str() {
            let actual = refusal["message"].as_str().unwrap();
            assert!(actual.contains(message), "{id}: {actual:?}");
        }
        assert_eq!(record["header"], Value::Null, "{id}");
        assert_eq!(record["sizeBytes"], Value::Null, "{id}");
        return;
    }
    assert_eq!(record.get("refusal"), None, "{id}");
    assert_eq!(record["path"], expected["path"], "{id}");
    assert_eq!(record["storedVersion"], expected["storedVersion"], "{id}");
    if outcome == "found" {
        assert_eq!(record["header"], expected["header"], "{id}");
        // `sizeBytes` is the selected file's size, following a link.
        let path = dir
            .join(case["root"].as_str().unwrap())
            .join(expected["path"].as_str().unwrap());
        let size = std::fs::metadata(path).unwrap().len();
        assert_eq!(record["sizeBytes"], size, "{id}");
    } else {
        assert_eq!(record["header"], Value::Null, "{id}");
        assert_eq!(record["sizeBytes"], Value::Null, "{id}");
    }
}

#[test]
fn stats_match_the_shared_table_and_leave_the_root_unchanged() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../conformance/session/stat-cases.json");
    let table: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(table["schema"], "bake/session-conformance/stat-cases");
    assert_eq!(table["version"], 1);
    let cases = table["cases"].as_array().unwrap();
    assert_eq!(cases.len(), CASE_COUNT);
    let mut ran = 0;
    for case in cases.iter().filter(|case| runs_here(case)) {
        let id = case["id"].as_str().unwrap();
        let scratch = Scratch::new();
        let (dir, home) = (scratch.0.join("case"), scratch.0.join("home"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&home).unwrap();
        build(&dir, &case["layout"]);
        let before = tree(&dir);
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
        command
            .args(["session", "stat", "--root"])
            .arg(case["root"].as_str().unwrap())
            .args(["--id", case["sessionId"].as_str().unwrap()])
            .args([
                "--max-header-bytes",
                &budget(&table, case, "maxHeaderBytes"),
            ])
            .args(["--max-entries", &budget(&table, case, "maxEntries")])
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(compression) = case.get("compression") {
            command.args(["--compression", compression.as_str().unwrap()]);
        }
        for name in ["HOME", "BAKE_HOME", "DSH_HOME", "USERPROFILE"] {
            command.env(name, &home);
        }
        let run = finish(command.spawn().unwrap());
        check(
            id,
            &dir,
            case,
            &run,
            case.get("rust").unwrap_or(&case["ts"]),
        );
        assert_eq!(tree(&dir), before, "{id}: the layout changed");
        assert_eq!(
            std::fs::read_dir(&home).unwrap().count(),
            0,
            "{id}: home written"
        );
        ran += 1;
    }
    let skipped = if cfg!(windows) { POSIX_ONLY } else { 0 };
    assert_eq!(ran, CASE_COUNT - skipped);
}

/// A FIFO named as the generation is refused as an irregular file without
/// waiting for a writer, which TypeScript's blocking open would do.
#[cfg(unix)]
#[test]
fn a_fifo_generation_fails_without_blocking() {
    let scratch = Scratch::new();
    let (dir, home) = (scratch.0.join("root/_no-cwd/a1"), scratch.0.join("home"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir(&home).unwrap();
    let fifo = dir.join("session.v3.jsonl");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `path` is a valid NUL-terminated string for the call's duration.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let run = finish(
        Command::new(env!("CARGO_BIN_EXE_bake-rs"))
            .args(["session", "stat", "--root", "root", "--id", "a1"])
            .args(["--max-header-bytes", "64", "--max-entries", "8"])
            .args(["--compression", "none"])
            .current_dir(&scratch.0)
            .env("HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert_eq!(run.status.code(), Some(1));
    assert!(run.stdout.is_empty());
    assert!(String::from_utf8_lossy(&run.stderr).contains("is not a regular file"));
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
}

/// Run `session stat` for `a1` in the owned `root` directory of `scratch`,
/// checking that the layout and an empty home are left unchanged.
fn stat_a1(scratch: &Scratch, compression: &str, max_header_bytes: usize) -> Value {
    let home = scratch.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let before = tree(&scratch.0.join("root"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
    command
        .args(["session", "stat", "--root", "root", "--id", "a1"])
        .args(["--max-header-bytes", &max_header_bytes.to_string()])
        .args(["--max-entries", "16", "--compression", compression])
        .current_dir(&scratch.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in ["HOME", "BAKE_HOME", "DSH_HOME", "USERPROFILE"] {
        command.env(name, &home);
    }
    let run = finish(command.spawn().unwrap());
    assert_eq!(tree(&scratch.0.join("root")), before, "the layout changed");
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0, "home written");
    assert!(
        run.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let record: Value = serde_json::from_slice(&run.stdout).unwrap();
    let status = if record["status"] == "refused" { 3 } else { 0 };
    assert_eq!(run.status.code(), Some(status), "{record}");
    record
}

/// A v3 header line whose `agentPreset` makes it span several 8 KiB reads.
fn long_header() -> (String, Value) {
    let preset = "p".repeat(20_000);
    let line = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"a1\",\"createdAt\":4000,\"isSeeded\":false,\"delegationDepth\":0,\"agentPreset\":\"{preset}\"}}\n"
    );
    let header = serde_json::json!({
        "version": 3, "id": "a1", "createdAt": 4000, "cwd": null, "parentSession": null,
        "isSeeded": false, "origin": null, "delegationDepth": 0, "agentPreset": preset,
    });
    (line, header)
}

/// One Zstandard frame holding `content` in 1-byte raw blocks, so its block
/// headers span many reads; `last` marks the final block as the frame's end.
fn tiny_block_frame(content: &[u8], last: bool) -> Vec<u8> {
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38];
    for (index, byte) in content.iter().enumerate() {
        let end = last && index + 1 == content.len();
        frame.extend([0x08 | u8::from(end), 0, 0, *byte]);
    }
    frame
}

fn write_generation(scratch: &Scratch, name: &str, bytes: &[u8]) {
    let dir = scratch.0.join("root/_no-cwd/a1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn assert_refused(record: &Value, reason: &str, kind: &str) {
    assert_eq!(record["status"], "refused", "{record}");
    assert_eq!(record["refusal"]["stage"], "header", "{record}");
    assert_eq!(record["refusal"]["reason"], reason, "{record}");
    assert_eq!(record["refusal"]["kind"], kind, "{record}");
}

#[test]
fn a_plain_header_spanning_several_reads_respects_its_budget() {
    let (line, header) = long_header();
    let mut bytes = line.clone().into_bytes();
    bytes.extend(b"not json\n".repeat(4_000));
    for budget in [line.len(), 1 << 20] {
        let scratch = Scratch::new();
        write_generation(&scratch, "session.v3.jsonl", &bytes);
        let record = stat_a1(&scratch, "none", budget);
        assert_eq!(record["status"], "found", "{record}");
        assert_eq!(record["header"], header);
        assert_eq!(record["sizeBytes"], bytes.len());
    }
    let scratch = Scratch::new();
    write_generation(&scratch, "session.v3.jsonl", &bytes);
    let record = stat_a1(&scratch, "none", line.len() - 1);
    assert_refused(&record, "header-budget", "native-limit");
}

#[test]
fn a_plain_file_without_lf_is_absent_at_its_end_or_refused_past_the_budget() {
    let bytes = vec![b'a'; 20_000];
    for (budget, status) in [(1 << 20, "absent"), (20_000, "absent")] {
        let scratch = Scratch::new();
        write_generation(&scratch, "session.v3.jsonl", &bytes);
        let record = stat_a1(&scratch, "none", budget);
        assert_eq!(record["status"], status, "{budget}: {record}");
        assert_eq!(record["path"], "_no-cwd/a1/session.v3.jsonl");
    }
    let scratch = Scratch::new();
    write_generation(&scratch, "session.v3.jsonl", &bytes);
    assert_refused(
        &stat_a1(&scratch, "none", 19_999),
        "header-budget",
        "native-limit",
    );
}

/// The first frame's 1-byte blocks put its end past several reads, so the
/// outcome must not depend on where the reads or frame checks fall.
#[test]
fn a_zstd_header_frame_of_many_tiny_blocks_is_read_to_its_end() {
    let (line, header) = long_header();
    let frame = tiny_block_frame(line.as_bytes(), true);
    let mut bytes = frame.clone();
    bytes.extend(vec![0xff; 200_000]);
    for budget in [frame.len(), 1 << 20] {
        let scratch = Scratch::new();
        write_generation(&scratch, "session.v3.jsonl.zstd", &bytes);
        let record = stat_a1(&scratch, "zstd", budget);
        assert_eq!(record["status"], "found", "{budget}: {record}");
        assert_eq!(record["header"], header);
        assert_eq!(record["sizeBytes"], bytes.len());
    }
    let scratch = Scratch::new();
    write_generation(&scratch, "session.v3.jsonl.zstd", &bytes);
    assert_refused(
        &stat_a1(&scratch, "zstd", frame.len() - 1),
        "header-budget",
        "native-limit",
    );
}

#[test]
fn a_truncated_or_corrupt_zstd_header_frame_is_decided_at_the_end_of_the_file() {
    let (line, _) = long_header();
    let truncated = tiny_block_frame(line.as_bytes(), false);
    for budget in [truncated.len(), 1 << 20] {
        let scratch = Scratch::new();
        write_generation(&scratch, "session.v3.jsonl.zstd", &truncated);
        let record = stat_a1(&scratch, "zstd", budget);
        assert_eq!(record["status"], "absent", "{budget}: {record}");
        assert_eq!(record["storedVersion"], 3);
    }
    // A reserved block type deep in the first frame, which ends the file
    // between frame checks.
    let mut corrupt = truncated[..truncated.len() - 4_000].to_vec();
    corrupt.extend([0x06, 0, 0]);
    let scratch = Scratch::new();
    write_generation(&scratch, "session.v3.jsonl.zstd", &corrupt);
    assert_refused(
        &stat_a1(&scratch, "zstd", 1 << 20),
        "corrupt-header-frame",
        "invalid",
    );
}
