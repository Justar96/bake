//! Runs the built `bake-rs session inspect` against the observations in
//! `session-inspect-expectations.json`, which were written before the command
//! first ran. Restored cases reuse the hand-written expectations of the shared
//! restoration and Zstd tables, mapped to the command's record here; nothing
//! is read from the command's own output.
//!
//! Each case owns a fresh directory and an empty home directory that every
//! child sees as `HOME`, `BAKE_HOME`, `DSH_HOME`, and `USERPROFILE`; the home
//! must still be empty and each input unchanged afterwards. Every child is
//! waited on with a deadline and killed and reaped when it passes.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value, json};

/// Process creation dominates; this bounds a hung child, never a result.
const DEADLINE: Duration = Duration::from_secs(60);

fn expectations() -> Value {
    serde_json::from_str(include_str!("session-inspect-expectations.json")).unwrap()
}

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn table(relative: &str) -> Value {
    serde_json::from_slice(&std::fs::read(repo_path(relative)).unwrap()).unwrap()
}

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bake-cli-inspect-{}-{name}-{count}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create an unused scratch directory");
        let scratch = Self(path);
        std::fs::create_dir(scratch.home()).unwrap();
        scratch
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    /// A new directory for one input, so its basename can be anything.
    fn dir(&self, name: impl AsRef<std::ffi::OsStr>) -> PathBuf {
        let dir = self.0.join(name.as_ref());
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn assert_home_unused(&self) {
        let entries = std::fs::read_dir(self.home()).unwrap().count();
        assert_eq!(entries, 0, "the command wrote to its home directory");
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

impl Run {
    fn code(&self) -> Option<i32> {
        self.status.code()
    }

    fn stderr(&self) -> String {
        String::from_utf8(self.stderr.clone()).unwrap()
    }

    fn record(&self) -> Value {
        let text = std::str::from_utf8(&self.stdout).unwrap();
        let line = text.strip_suffix('\n').expect("one LF-terminated record");
        assert!(!line.contains('\n'), "one line: {text}");
        serde_json::from_str(line).unwrap()
    }
}

fn command(scratch: &Scratch, args: &[OsString]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
    command
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped());
    for name in ["HOME", "BAKE_HOME", "DSH_HOME", "USERPROFILE"] {
        command.env(name, scratch.home());
    }
    command
}

/// Wait for the child, draining its piped streams. At the deadline, or when
/// polling fails, the child is killed and reaped and both drains are joined
/// before the test fails, so no child or thread outlives the call.
fn finish(mut child: Child) -> Run {
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                stream.read_to_end(&mut bytes)?;
            }
            Ok::<_, std::io::Error>(bytes)
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
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

fn run_in(scratch: &Scratch, args: &[OsString], cwd: Option<&Path>) -> Run {
    let mut command = command(scratch, args);
    command.stdout(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let run = finish(command.spawn().unwrap());
    scratch.assert_home_unused();
    run
}

fn run(scratch: &Scratch, args: &[OsString]) -> Run {
    run_in(scratch, args, None)
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn inspect_args(max_bytes: &Value, max_source_seqs: &Value, path: &Path) -> Vec<OsString> {
    let mut args = os(&["session", "inspect", "--max-bytes"]);
    args.push(max_bytes.to_string().into());
    args.push("--max-source-seqs".into());
    args.push(max_source_seqs.to_string().into());
    args.push(path.into());
    args
}

/// A file and its observed state, checked unchanged after the run.
struct Input {
    path: PathBuf,
    bytes: Vec<u8>,
    modified: SystemTime,
}

impl Input {
    fn write(dir: &Path, name: &str, bytes: &[u8]) -> Self {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        Self {
            path,
            bytes: bytes.to_vec(),
            modified,
        }
    }

    fn assert_unchanged(&self) {
        assert_eq!(std::fs::read(&self.path).unwrap(), self.bytes);
        let modified = std::fs::metadata(&self.path).unwrap().modified().unwrap();
        assert_eq!(modified, self.modified);
    }
}

// ---------------------------------------------------------------- shared tables

/// A restoration case's bytes, built as `tests/restore_cases.rs` builds them,
/// with the optional header substitution an expectation adds.
fn restore_case(id: &str, header_edit: Option<(&str, &str)>) -> (Vec<u8>, Map<String, Value>) {
    let restore = table("conformance/session/restore-cases.json");
    let case = restore["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("no restoration case {id}"))
        .as_object()
        .unwrap()
        .clone();
    let log = restore["logs"][case["log"].as_str().unwrap()]
        .as_str()
        .unwrap();
    let source = String::from_utf8(std::fs::read(repo_path(log)).unwrap()).unwrap();
    let mut lines: Vec<String> = source
        .strip_suffix('\n')
        .unwrap()
        .split('\n')
        .map(str::to_owned)
        .collect();
    let mut header = lines.remove(0);
    let mut rows = lines;
    let mut tail = String::new();
    for edit in case["edits"].as_array().unwrap() {
        let text = |key: &str| edit[key].as_str().unwrap().to_owned();
        let row = || usize::try_from(edit["row"].as_u64().unwrap()).unwrap();
        let mut keys: Vec<&str> = edit
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        match keys.as_slice() {
            ["truncate"] => {
                rows.truncate(usize::try_from(edit["truncate"].as_u64().unwrap()).unwrap())
            }
            ["header"] => header = text("header"),
            ["append"] => rows.push(text("append")),
            ["tail"] => tail = text("tail"),
            ["row", "text"] => rows[row()] = text("text"),
            ["find", "replace", "row"] => {
                let row = row();
                assert_eq!(rows[row].matches(&text("find")).count(), 1, "{id}");
                rows[row] = rows[row].replacen(&text("find"), &text("replace"), 1);
            }
            other => panic!("{id}: edit {other:?}"),
        }
    }
    if let Some((find, replace)) = header_edit {
        assert_eq!(header.matches(find).count(), 1, "{id}: header edit");
        header = header.replacen(find, replace, 1);
    }
    let mut log = String::new();
    for line in std::iter::once(&header).chain(&rows) {
        log.push_str(line);
        log.push('\n');
    }
    log.push_str(&tail);
    (log.into_bytes(), case)
}

fn zstd_case(id: &str) -> (Vec<u8>, Map<String, Value>) {
    let zstd = table("conformance/session/zstd-cases.json");
    let case = zstd["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("no Zstd case {id}"))
        .as_object()
        .unwrap()
        .clone();
    let hex = case["hex"].as_str().unwrap();
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    (bytes, case)
}

/// The input an expectation names, and the case it came from.
fn case_input(expectation: &Value) -> (Vec<u8>, Map<String, Value>) {
    let id = expectation["case"].as_str().unwrap();
    match expectation["table"].as_str().unwrap() {
        "restore" => {
            let header_edit = expectation["headerFind"]
                .as_str()
                .map(|find| (find, expectation["headerReplace"].as_str().unwrap()));
            restore_case(id, header_edit)
        }
        "zstd" => zstd_case(id),
        other => panic!("unknown table {other}"),
    }
}

/// The table's expected restored state, in this command's record form.
fn restored_record(expectation: &Value, input: &[u8], case: &Map<String, Value>) -> Value {
    let (encoding, state, torn) = if expectation["table"] == "restore" {
        assert!(
            !case.contains_key("rust"),
            "the TypeScript expectation applies"
        );
        (
            "none",
            case["ts"].as_object().unwrap(),
            expectation["torn"].clone(),
        )
    } else {
        let state = case.get("rust").unwrap_or(&case["expected"]);
        let rows = state["rows"].as_array().unwrap().len() as u64;
        let torn = match &state["torn"] {
            Value::Null => Value::Null,
            tail => json!({
                "truncateTo": tail["truncateTo"],
                "recoveredFrom": tail["recoveredFrom"],
                "recoveredEventCount": rows - tail["recoveredFrom"].as_u64().unwrap(),
            }),
        };
        ("zstd", state.as_object().unwrap(), torn)
    };
    assert_eq!(state["outcome"], "restored");
    let header = &state["header"];
    let stored_events = state
        .get("storedEventCount")
        .cloned()
        .unwrap_or_else(|| json!(state["rows"].as_array().unwrap().len()));
    let closer_types: Vec<&Value> = state["closers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|closer| &closer["type"])
        .collect();
    let optional = |key: &str| header.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "status": "restored",
        "encoding": encoding,
        "formatVersion": 3,
        "fileBytes": input.len(),
        "header": {
            "id": header["id"],
            "createdAt": header["createdAt"],
            "cwd": optional("cwd"),
            "parentSession": optional("parentSession"),
            "isSeeded": header["isSeeded"],
            "origin": optional("origin"),
            "delegationDepth": header["delegationDepth"],
            "agentPreset": optional("agentPreset"),
        },
        "storedEventCount": stored_events,
        "committedPlaintextBytes": state["committedBytes"],
        "inheritedEventCount": state["inheritedEventCount"],
        "torn": torn,
        "repair": {"closerTypes": closer_types, "endSeedAppended": state["endSeedAppended"]},
        "projection": {
            "messageCount": state["messages"].as_array().unwrap().len(),
            "hasRequestHeader": !state["requestHeader"].is_null(),
            "hasRequestContext": !state["requestContext"].is_null(),
        },
    })
}

fn with_path(template: &str, path: &Path) -> String {
    template.replace("{path}", &format!("{path:?}"))
}

// ---------------------------------------------------------------- restored and refused logs

#[test]
fn restored_logs_print_the_tables_expected_state() {
    let expectations = expectations();
    let cases = expectations["restored"].as_array().unwrap();
    assert_eq!(cases.len(), 9);
    for expectation in cases {
        let id = expectation["id"].as_str().unwrap();
        let scratch = Scratch::new(id);
        let (bytes, case) = case_input(expectation);
        let input = Input::write(
            &scratch.dir("in"),
            expectation["name"].as_str().unwrap(),
            &bytes,
        );
        let out = run(
            &scratch,
            &inspect_args(
                &expectation["maxBytes"],
                &expectation["maxSourceSeqs"],
                &input.path,
            ),
        );
        assert_eq!(out.code(), Some(0), "{id}: {}", out.stderr());
        assert!(out.stderr.is_empty(), "{id}");
        assert_eq!(
            out.record(),
            restored_record(expectation, &bytes, &case),
            "{id}"
        );
        input.assert_unchanged();
    }
}

#[test]
fn refused_logs_print_one_refusal_record_with_status_3() {
    let expectations = expectations();
    let cases = expectations["refused"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    for expectation in cases {
        let id = expectation["id"].as_str().unwrap();
        let scratch = Scratch::new(id);
        let (bytes, _) = case_input(expectation);
        let input = Input::write(
            &scratch.dir("in"),
            expectation["name"].as_str().unwrap(),
            &bytes,
        );
        let out = run(
            &scratch,
            &inspect_args(
                &expectation["maxBytes"],
                &expectation["maxSourceSeqs"],
                &input.path,
            ),
        );
        assert_eq!(out.code(), Some(3), "{id}: {}", out.stderr());
        assert!(out.stderr.is_empty(), "{id}");
        let mut expected = expectation["stdout"].clone();
        if expected["fileBytes"] == "$input" {
            expected["fileBytes"] = json!(bytes.len());
        }
        assert_eq!(out.record(), expected, "{id}");
        input.assert_unchanged();
    }
}

#[test]
fn records_escape_control_characters_and_keep_other_unicode() {
    let case = &expectations()["escaping"];
    let scratch = Scratch::new("escaping");
    let input = Input::write(
        &scratch.dir("in"),
        case["name"].as_str().unwrap(),
        case["log"].as_str().unwrap().as_bytes(),
    );
    let out = run(
        &scratch,
        &inspect_args(&case["maxBytes"], &case["maxSourceSeqs"], &input.path),
    );
    assert_eq!(out.code(), Some(0), "{}", out.stderr());
    assert_eq!(
        String::from_utf8(out.stdout.clone()).unwrap(),
        case["stdout"].as_str().unwrap()
    );
    assert!(!out.stdout.iter().any(|byte| *byte < 0x20 && *byte != b'\n'));
    input.assert_unchanged();
}

// ---------------------------------------------------------------- file failures

#[test]
fn oversized_files_are_refused_before_restoration() {
    let expectations = expectations();
    for expectation in expectations["fileFailures"].as_array().unwrap() {
        if expectation.get("table").is_none() {
            continue;
        }
        let id = expectation["id"].as_str().unwrap();
        let scratch = Scratch::new(id);
        let (bytes, _) = case_input(expectation);
        let input = Input::write(
            &scratch.dir("in"),
            expectation["name"].as_str().unwrap(),
            &bytes,
        );
        let out = run(
            &scratch,
            &inspect_args(&expectation["maxBytes"], &json!(64), &input.path),
        );
        assert_eq!(out.code(), Some(1), "{id}");
        assert!(out.stdout.is_empty(), "{id}");
        assert_eq!(
            out.stderr(),
            with_path(expectation["stderr"].as_str().unwrap(), &input.path),
            "{id}"
        );
        input.assert_unchanged();
    }
}

fn file_failure(id: &str) -> Value {
    expectations()["fileFailures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap()
        .clone()
}

#[test]
fn directories_and_missing_files_are_refused() {
    let scratch = Scratch::new("not-files");
    let directory = file_failure("directory");
    let path = scratch.dir("in").join(directory["name"].as_str().unwrap());
    std::fs::create_dir(&path).unwrap();
    let out = run(&scratch, &inspect_args(&json!(4096), &json!(64), &path));
    assert_eq!(out.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        out.stderr(),
        with_path(directory["stderr"].as_str().unwrap(), &path)
    );

    let missing = file_failure("missing");
    let path = scratch.dir("gone").join(missing["name"].as_str().unwrap());
    let out = run(&scratch, &inspect_args(&json!(4096), &json!(64), &path));
    assert_eq!(out.code(), Some(1));
    assert!(out.stdout.is_empty());
    let stderr = out.stderr();
    let prefix = with_path(missing["stderrPrefix"].as_str().unwrap(), &path);
    assert!(stderr.starts_with(&prefix), "{stderr}");
    assert!(
        stderr.ends_with('\n') && stderr.lines().count() == 1,
        "{stderr}"
    );
}

/// A FIFO with no writer would block an ordinary open; the deadline turns a
/// hang into a failure rather than deciding the result.
#[cfg(unix)]
#[test]
fn fifos_are_refused_without_waiting_for_a_writer() {
    let scratch = Scratch::new("fifo");
    let fifo = file_failure("fifo");
    let path = scratch.dir("in").join(fifo["name"].as_str().unwrap());
    let made = Command::new("mkfifo")
        .arg(&path)
        .stdin(Stdio::null())
        .status()
        .expect("mkfifo runs");
    assert!(made.success());
    let out = run(&scratch, &inspect_args(&json!(4096), &json!(64), &path));
    assert_eq!(out.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        out.stderr(),
        with_path(fifo["stderr"].as_str().unwrap(), &path)
    );
}

// ---------------------------------------------------------------- names

#[test]
fn only_current_format_names_are_opened() {
    let expectations = expectations();
    let names = &expectations["names"];
    let mut cases: Vec<(OsString, String)> = Vec::new();
    for (group, template) in [("legacy", "legacyStderr"), ("newer", "newerStderr")] {
        for entry in names[group].as_array().unwrap() {
            let message = names[template]
                .as_str()
                .unwrap()
                .replace("{version}", &entry[1].to_string());
            cases.push((entry[0].as_str().unwrap().into(), message));
        }
    }
    let noncanonical = names["noncanonicalStderr"].as_str().unwrap();
    for name in names["noncanonical"].as_array().unwrap() {
        cases.push((name.as_str().unwrap().into(), noncanonical.to_owned()));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;
        cases.push((
            OsString::from_vec(b"session.v3.jsonl\xff".to_vec()),
            noncanonical.to_owned(),
        ));
    }
    let scratch = Scratch::new("names");
    for (index, (name, template)) in cases.iter().enumerate() {
        // Each case has its own parents: case variants such as `Session.v3.jsonl`
        // and `session.v3.JSONL` collide on case-insensitive filesystems.
        let missing = scratch.dir(format!("missing-{index}")).join(name);
        let directory = scratch.dir(format!("directory-{index}")).join(name);
        std::fs::create_dir(&directory).unwrap();
        // Neither a missing file nor a directory is reported: the name decides first.
        for path in [missing, directory] {
            let out = run(&scratch, &inspect_args(&json!(4096), &json!(64), &path));
            assert_eq!(out.code(), Some(1), "{name:?}");
            assert!(out.stdout.is_empty(), "{name:?}");
            assert_eq!(out.stderr(), with_path(template, &path), "{name:?}");
        }
    }
}

#[test]
fn unicode_and_dash_paths_are_opened_as_given() {
    let (bytes, case) = restore_case("tool-call-turn", None);
    let expectation = &expectations()["restored"][0];
    assert_eq!(expectation["case"], "tool-call-turn");
    let expected = restored_record(expectation, &bytes, &case);
    let scratch = Scratch::new("paths");

    let input = Input::write(&scratch.dir("检查 é"), "session.v3.jsonl", &bytes);
    let out = run(
        &scratch,
        &inspect_args(&json!(4533), &json!(64), &input.path),
    );
    assert_eq!(out.code(), Some(0), "{}", out.stderr());
    assert_eq!(out.record(), expected);
    input.assert_unchanged();

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;
        let parent = scratch.dir(OsString::from_vec(b"not-utf8-\xff".to_vec()));
        let input = Input::write(&parent, "session.v3.jsonl", &bytes);
        let out = run(
            &scratch,
            &inspect_args(&json!(4533), &json!(64), &input.path),
        );
        assert_eq!(out.code(), Some(0), "{}", out.stderr());
        assert_eq!(out.record(), expected);
        input.assert_unchanged();
    }

    // The child alone runs in the scratch directory, so the relative path starts with '-'.
    let input = Input::write(&scratch.dir("-dash"), "session.v3.jsonl", &bytes);
    let relative = Path::new("-dash").join("session.v3.jsonl");
    let mut args = os(&[
        "session",
        "inspect",
        "--max-bytes",
        "4533",
        "--max-source-seqs",
        "64",
        "--",
    ]);
    args.push(relative.into());
    let out = run_in(&scratch, &args, Some(&scratch.0));
    assert_eq!(out.code(), Some(0), "{}", out.stderr());
    assert_eq!(out.record(), expected);
    input.assert_unchanged();
}

// ---------------------------------------------------------------- arguments and streams

#[test]
fn usage_errors_exit_2_and_point_to_the_inspect_help() {
    let expectations = expectations();
    let usage = &expectations["usage"];
    let template = usage["stderr"].as_str().unwrap();
    let mut cases: Vec<(Vec<OsString>, String)> = usage["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let args = case[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| OsString::from(arg.as_str().unwrap()))
                .collect();
            (args, case[1].as_str().unwrap().to_owned())
        })
        .collect();
    for option in ["--max-bytes", "--max-source-seqs"] {
        let other = if option == "--max-bytes" {
            "--max-source-seqs"
        } else {
            "--max-bytes"
        };
        for value in usage["badValues"].as_array().unwrap() {
            let value = value.as_str().unwrap();
            let message = usage["badValueMessage"]
                .as_str()
                .unwrap()
                .replace("{option}", option)
                .replace("{value}", value);
            cases.push((
                os(&[
                    "session",
                    "inspect",
                    other,
                    "1",
                    option,
                    value,
                    "f/session.v3.jsonl",
                ]),
                message,
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let case = &usage["nonUtf8Value"];
        let mut args = os(&[
            "session",
            "inspect",
            "--max-source-seqs",
            "1",
            case[0].as_str().unwrap(),
        ]);
        args.push(OsString::from_vec(vec![0xff]));
        args.push("f/session.v3.jsonl".into());
        cases.push((args, case[1].as_str().unwrap().to_owned()));
    }
    let scratch = Scratch::new("usage");
    for (args, message) in &cases {
        let out = run(&scratch, args);
        assert_eq!(out.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert_eq!(
            out.stderr(),
            template.replace("{message}", message),
            "{args:?}"
        );
    }

    // The largest budget is accepted; only the missing file fails.
    let largest = usage["largestValue"].as_str().unwrap();
    let path = scratch.dir("largest").join("session.v3.jsonl");
    let mut args = os(&[
        "session",
        "inspect",
        "--max-bytes",
        largest,
        "--max-source-seqs",
        largest,
    ]);
    args.push(path.clone().into());
    let out = run(&scratch, &args);
    assert_eq!(out.code(), Some(1));
    assert!(
        out.stderr()
            .starts_with(&format!("bake-rs: cannot open {path:?}: "))
    );
}

#[test]
fn help_names_the_inspect_command() {
    let scratch = Scratch::new("help");
    let out = run(&scratch, &os(&["--help"]));
    assert_eq!(out.code(), Some(0));
    let help = String::from_utf8(out.stdout).unwrap();
    assert!(help.contains("bake-rs preview"));
    assert!(
        help.contains("bake-rs session inspect --max-bytes <N> --max-source-seqs <N> [--] <file>")
    );
    for args in [
        &["session", "--help"][..],
        &["session", "inspect", "--help"],
        &["session", "inspect", "-h"],
    ] {
        let out = run(&scratch, &os(args));
        assert_eq!(out.code(), Some(0), "{args:?}");
        assert!(out.stderr.is_empty());
        let help = String::from_utf8(out.stdout).unwrap();
        assert!(help.starts_with("bake-rs session inspect: "), "{help}");
        assert!(
            help.contains("Exit status: 0 restored, 3 refused log"),
            "{help}"
        );
    }
}

/// The read end is closed before the child starts, so every write fails.
#[test]
fn a_closed_stdout_exits_1_without_a_panic() {
    let expectations = expectations();
    for expectation in [&expectations["restored"][0], &expectations["refused"][0]] {
        let id = expectation["id"].as_str().unwrap();
        let scratch = Scratch::new(id);
        let (bytes, _) = case_input(expectation);
        let input = Input::write(
            &scratch.dir("in"),
            expectation["name"].as_str().unwrap(),
            &bytes,
        );
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut command = command(
            &scratch,
            &inspect_args(
                &expectation["maxBytes"],
                &expectation["maxSourceSeqs"],
                &input.path,
            ),
        );
        command.stdout(writer);
        let child = command.spawn().unwrap();
        drop(command);
        let out = finish(child);
        scratch.assert_home_unused();
        assert_eq!(out.code(), Some(1), "{id}");
        assert!(out.stderr.is_empty(), "{id}: {}", out.stderr());
        input.assert_unchanged();
    }
}
