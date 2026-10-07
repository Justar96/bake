//! Runs the built `bake-rs session inspect --root --id` against every case of
//! `conformance/session/lookup-cases.json` that applies to this host. The
//! table's expectations were written from the TypeScript sources before
//! either harness ran, and its TypeScript spec checks the same layouts
//! through the production backend; a `rust` entry replaces the expectation
//! only where this preview reports a native limit or budget. Layouts are
//! built here independently of that spec.
//!
//! Each case owns a fresh directory, run as the child's working directory so
//! relative roots resolve against it, and an empty home directory that every
//! child sees as `HOME`, `BAKE_HOME`, `DSH_HOME`, and `USERPROFILE`. Every
//! entry under the case directory, links included, must be unchanged after
//! the run, and the home must stay empty. Each child is waited on with a
//! deadline and killed and reaped when it passes.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// Process creation dominates; this bounds a hung child, never a result.
const DEADLINE: Duration = Duration::from_secs(60);
const CASE_COUNT: usize = 88;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn read_table(relative: &str) -> Value {
    serde_json::from_slice(&std::fs::read(repo_path(relative)).unwrap()).unwrap()
}

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bake-cli-lookup-{}-{name}-{count}",
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

struct Run {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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

// ---------------------------------------------------------------- layouts

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

struct Tables {
    lookup: Value,
    zstd: Value,
}

impl Tables {
    fn input(&self, name: &str) -> String {
        let path = self.lookup["inputs"][name]["path"].as_str().unwrap();
        String::from_utf8(std::fs::read(repo_path(path)).unwrap()).unwrap()
    }

    fn log(&self, spec: &Value) -> Vec<u8> {
        let text = self.input(spec["input"].as_str().unwrap());
        let mut lines: Vec<String> = text
            .strip_suffix('\n')
            .expect("an input ends with LF")
            .split('\n')
            .map(str::to_owned)
            .collect();
        let mut header = lines.remove(0);
        if let Some(replacement) = spec.get("header") {
            replacement.as_str().unwrap().clone_into(&mut header);
        }
        let mut rows = lines;
        for edit in spec
            .get("edits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let row = usize::try_from(edit["row"].as_u64().unwrap()).unwrap();
            if let Some(text) = edit.get("text") {
                rows[row] = text.as_str().unwrap().to_owned();
            } else {
                let (find, replace) = (
                    edit["find"].as_str().unwrap(),
                    edit["replace"].as_str().unwrap(),
                );
                assert_eq!(rows[row].matches(find).count(), 1, "edit of row {row}");
                rows[row] = rows[row].replacen(find, replace, 1);
            }
        }
        let body: String = rows.iter().map(|row| format!("{row}\n")).collect();
        if spec
            .get("encode")
            .is_some_and(|encode| encode == "zstd-raw")
        {
            assert!(spec.get("tail").is_none(), "a raw-frame log takes no tail");
            let mut bytes = raw_frame(format!("{header}\n").as_bytes());
            bytes.extend(raw_frame(body.as_bytes()));
            return bytes;
        }
        let tail = spec.get("tail").and_then(Value::as_str).unwrap_or("");
        format!("{header}\n{body}{tail}").into_bytes()
    }

    fn file(&self, entry: &Value) -> Vec<u8> {
        if let Some(text) = entry.get("text") {
            return text.as_str().unwrap().as_bytes().to_vec();
        }
        if let Some(id) = entry.get("zstdCase") {
            let case = self.zstd["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| &case["id"] == id)
                .expect("a Zstd case");
            return hex(case["hex"].as_str().unwrap());
        }
        if let Some(frames) = entry.get("frames") {
            let mut bytes: Vec<u8> = frames
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|frame| raw_frame(frame.as_str().unwrap().as_bytes()))
                .collect();
            bytes.extend(hex(entry
                .get("appendHex")
                .and_then(Value::as_str)
                .unwrap_or("")));
            return bytes;
        }
        self.log(&entry["log"])
    }

    fn build(&self, dir: &Path, layout: &Value) {
        for entry in layout.as_array().unwrap() {
            let at = |key: &str| dir.join(entry[key].as_str().unwrap());
            if entry.get("dir").is_some() {
                std::fs::create_dir_all(at("dir")).unwrap();
                if let Some(name) = entry.get("rawNameHex") {
                    raw_dir(&at("dir"), &hex(name.as_str().unwrap()));
                }
            } else if entry.get("symlink").is_some() {
                let link = at("symlink");
                std::fs::create_dir_all(link.parent().unwrap()).unwrap();
                symlink(entry["target"].as_str().unwrap(), &link);
            } else if entry.get("hardlink").is_some() {
                let link = at("hardlink");
                std::fs::create_dir_all(link.parent().unwrap()).unwrap();
                std::fs::hard_link(at("target"), &link).unwrap();
            } else {
                let path = at("file");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, self.file(entry)).unwrap();
            }
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

#[cfg(target_os = "linux")]
fn raw_dir(parent: &Path, name: &[u8]) {
    use std::os::unix::ffi::OsStrExt;
    std::fs::create_dir(parent.join(std::ffi::OsStr::from_bytes(name))).unwrap();
}

#[cfg(not(target_os = "linux"))]
fn raw_dir(_: &Path, _: &[u8]) {
    unreachable!("raw names are created only on Linux");
}

/// Every entry under `dir`, not following links: kind, bytes or link
/// target, and a file's modification time.
fn snapshot(
    dir: &Path,
    prefix: &Path,
    out: &mut Vec<(PathBuf, String, Vec<u8>, Option<SystemTime>)>,
) {
    let mut names: Vec<OsString> = std::fs::read_dir(dir)
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
                "link".into(),
                target.into_os_string().into_encoded_bytes(),
                None,
            ));
        } else if metadata.is_dir() {
            out.push((relative.clone(), "dir".into(), Vec::new(), None));
            snapshot(&path, &relative, out);
        } else {
            out.push((
                relative,
                "file".into(),
                std::fs::read(&path).unwrap(),
                metadata.modified().ok(),
            ));
        }
    }
}

fn tree(dir: &Path) -> Vec<(PathBuf, String, Vec<u8>, Option<SystemTime>)> {
    let mut out = Vec::new();
    snapshot(dir, Path::new(""), &mut out);
    out
}

fn runs_here(case: &Value) -> bool {
    let Some(platforms) = case.get("platforms") else {
        return true;
    };
    platforms
        .as_array()
        .unwrap()
        .iter()
        .any(|platform| match platform.as_str().unwrap() {
            "linux" => cfg!(target_os = "linux"),
            "win32" => cfg!(windows),
            "posix" => cfg!(not(windows)),
            other => panic!("unknown platform {other}"),
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
fn check(id: &str, root: &Path, run: &Run, expected: &Value, diagnostic: Option<&Value>) {
    let stderr = String::from_utf8_lossy(&run.stderr);
    let code = run.status.code();
    if expected["outcome"] == "failure" {
        assert_eq!(code, Some(1), "{id}: {stderr}");
        assert!(run.stdout.is_empty(), "{id}");
        let diagnostic =
            diagnostic.unwrap_or_else(|| panic!("{id}: a failure names its diagnostic"));
        let line = stderr
            .strip_prefix("bake-rs: ")
            .and_then(|line| line.strip_suffix('\n'))
            .unwrap_or_else(|| panic!("{id}: one diagnostic line, {stderr}"));
        assert!(!line.contains('\n'), "{id}: {stderr}");
        assert!(
            line.contains(diagnostic["text"].as_str().unwrap()),
            "{id}: {stderr}"
        );
        // The diagnostic quotes the absolute path, which ends with the
        // case-relative one; the working directory's own spelling may differ.
        let relative: PathBuf = diagnostic["path"].as_str().unwrap().split('/').collect();
        let quoted = format!("{relative:?}");
        let tail = format!("{}\"", &quoted[1..quoted.len() - 1]);
        assert!(line.contains(&tail), "{id}: {stderr} lacks {tail}");
        return;
    }
    assert!(
        diagnostic.is_none(),
        "{id}: only a failure names a diagnostic"
    );
    let text = std::str::from_utf8(&run.stdout).unwrap();
    let line = text
        .strip_suffix('\n')
        .unwrap_or_else(|| panic!("{id}: one record, {stderr}"));
    let record: Value = serde_json::from_str(line).unwrap();
    if expected["outcome"] == "restored" {
        assert_eq!(code, Some(0), "{id}: {record}");
        let torn = match &record["torn"] {
            Value::Null => Value::Null,
            torn => {
                serde_json::json!({"truncateTo": torn["truncateTo"], "recoveredFrom": torn["recoveredFrom"]})
            }
        };
        let observed = serde_json::json!({
            "outcome": "restored",
            "path": record["path"],
            "header": {"id": record["header"]["id"], "cwd": record["header"]["cwd"]},
            "storedEventCount": record["storedEventCount"],
            "closerCount": record["repair"]["closerTypes"].as_array().unwrap().len(),
            "messageCount": record["projection"]["messageCount"],
            "inheritedEventCount": record["inheritedEventCount"],
            "endSeedAppended": record["repair"]["endSeedAppended"],
            "torn": torn,
        });
        assert_eq!(&observed, expected, "{id}");
        assert_eq!(record["status"], "restored", "{id}");
        assert!(record["fileBytes"].is_u64(), "{id}");
        return;
    }
    assert_eq!(code, Some(3), "{id}: {record}");
    assert_eq!(record["status"], "refused", "{id}");
    let refusal = &record["refusal"];
    for key in ["stage", "reason", "kind", "path"] {
        assert_eq!(refusal[key], expected[key], "{id}: {key} of {record}");
    }
    if let Some(message) = expected.get("message") {
        assert_eq!(&refusal["message"], message, "{id}");
    }
    // Only a selected generation has a path, and only one that exists and
    // is not refused by its version alone has been read.
    let stage = expected["stage"].as_str().unwrap();
    let selected = !matches!(stage, "root" | "layout" | "lookup");
    let read = selected
        && expected["reason"] != "migration-required"
        && root.join(record["path"].as_str().unwrap()).exists();
    assert_eq!(record["fileBytes"].is_u64(), read, "{id}: {record}");
    assert_eq!(record["path"].is_string(), selected, "{id}: {record}");
}

#[test]
fn lookups_reach_the_shared_tables_stages_and_leave_the_root_unchanged() {
    let tables = Tables {
        lookup: read_table("conformance/session/lookup-cases.json"),
        zstd: read_table("conformance/session/zstd-cases.json"),
    };
    assert_eq!(
        tables.lookup["schema"],
        "bake/session-conformance/lookup-cases"
    );
    assert_eq!(tables.lookup["version"], 3);
    let cases = tables.lookup["cases"].as_array().unwrap();
    assert_eq!(cases.len(), CASE_COUNT);
    let probe = Scratch::new("probe");
    std::fs::write(probe.0.join("probe-case"), "").unwrap();
    let case_insensitive = std::fs::symlink_metadata(probe.0.join("PROBE-CASE")).is_ok();
    let mut ran = 0;
    for case in cases.iter().filter(|case| runs_here(case)) {
        let id = case["id"].as_str().unwrap();
        let scratch = Scratch::new("case");
        let (dir, home) = (scratch.0.join("case"), scratch.0.join("home"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&home).unwrap();
        tables.build(&dir, &case["layout"]);
        let before = tree(&dir);
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
        command
            .args(["session", "inspect", "--root"])
            .arg(case["root"].as_str().unwrap())
            .args(["--id", case["sessionId"].as_str().unwrap()])
            .args(["--max-bytes", &budget(&tables.lookup, case, "maxBytes")])
            .args([
                "--max-source-seqs",
                &budget(&tables.lookup, case, "maxSourceSeqs"),
            ])
            .args(["--max-entries", &budget(&tables.lookup, case, "maxEntries")])
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
        let expected = match case.get("whenCaseInsensitive") {
            Some(insensitive) if case_insensitive => insensitive,
            _ => case.get("rust").unwrap_or(&case["ts"]),
        };
        let root = dir.join(case["root"].as_str().unwrap());
        check(id, &root, &run, expected, case.get("nativeDiagnostic"));
        assert_eq!(tree(&dir), before, "{id}: the layout changed");
        assert_eq!(
            std::fs::read_dir(&home).unwrap().count(),
            0,
            "{id}: home written"
        );
        ran += 1;
    }
    let expected_runs = if cfg!(target_os = "linux") {
        84
    } else if cfg!(windows) {
        63
    } else {
        81
    };
    assert_eq!(ran, expected_runs);
}

/// Run the lookup form with the given root and working directory, and an
/// empty owned home; the record or diagnostic is returned.
#[cfg(any(target_os = "linux", windows))]
fn lookup_in(cwd: &Path, home: &Path, root: &std::ffi::OsStr) -> Run {
    lookup_in_with_id(cwd, home, root, "a1")
}

#[cfg(any(target_os = "linux", windows))]
fn lookup_in_with_id(cwd: &Path, home: &Path, root: &std::ffi::OsStr, id: &str) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
    command
        .args(["session", "inspect", "--root"])
        .arg(root)
        .args(["--id", id, "--max-bytes", "64", "--max-source-seqs", "1"])
        .args(["--max-entries", "64"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in ["HOME", "BAKE_HOME", "DSH_HOME", "USERPROFILE"] {
        command.env(name, home);
    }
    let run = finish(command.spawn().unwrap());
    assert_eq!(std::fs::read_dir(home).unwrap().count(), 0, "home written");
    run
}

#[cfg(any(target_os = "linux", windows))]
fn root_refusal(run: &Run) -> Value {
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(3), "{stderr}");
    let record: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(record["fileBytes"], Value::Null);
    assert_eq!(record["path"], Value::Null);
    record["refusal"].clone()
}

/// Node's `process.cwd()` and `path.resolve` produce JavaScript strings, so a
/// working directory that is not UTF-8 is outside what the shared table can
/// express; the lookup refuses it before listing anything.
#[cfg(target_os = "linux")]
#[test]
fn a_relative_root_under_a_non_utf8_working_directory_is_a_native_limit() {
    use std::os::unix::ffi::OsStrExt;
    let scratch = Scratch::new("non-utf8-cwd");
    let cwd = scratch.0.join(std::ffi::OsStr::from_bytes(b"w\xff"));
    let home = scratch.0.join("home");
    std::fs::create_dir_all(cwd.join("root/_no-cwd/a1")).unwrap();
    std::fs::create_dir(&home).unwrap();
    let refusal = root_refusal(&lookup_in(&cwd, &home, "root".as_ref()));
    assert_eq!(refusal["stage"], "root");
    assert_eq!(refusal["reason"], "non-utf8-name");
    assert_eq!(refusal["kind"], "native-limit");
}

/// Roots whose Win32 resolution this preview does not share with Node are
/// refused before any listing; ordinary relative and drive-absolute roots
/// are covered by the shared cases.
#[cfg(windows)]
#[test]
fn windows_root_spellings_outside_the_supported_forms_are_native_limits() {
    let scratch = Scratch::new("windows-roots");
    let home = scratch.0.join("home");
    std::fs::create_dir(&home).unwrap();
    let absolute = std::path::absolute(&scratch.0).unwrap();
    let absolute = absolute.to_str().unwrap();
    let drive = &absolute[..2];
    for root in [
        format!(r"\\?\{absolute}"),
        format!(r"\\.\{absolute}"),
        format!("{drive}root"),
        absolute[2..].to_owned(),
        r"\\localhost\share\root".to_owned(),
        format!(r"{absolute}\root."),
        format!(r"{absolute}\root "),
    ] {
        let refusal = root_refusal(&lookup_in(&scratch.0, &home, root.as_ref()));
        assert_eq!(refusal["stage"], "root", "{root}");
        assert_eq!(refusal["reason"], "unsupported-root", "{root}");
        assert_eq!(refusal["kind"], "native-limit", "{root}");
    }
}

#[cfg(windows)]
#[test]
fn windows_lookup_refuses_names_that_ordinary_paths_can_normalize() {
    let scratch = Scratch::new("windows-id-names");
    let home = scratch.0.join("home");
    std::fs::create_dir(&home).unwrap();
    for id in ["a.", "CON", "nul.txt", "COM1", "LPT9"] {
        let refusal = root_refusal(&lookup_in_with_id(
            &scratch.0,
            &home,
            "missing".as_ref(),
            id,
        ));
        assert_eq!(refusal["stage"], "lookup", "{id}");
        assert_eq!(refusal["reason"], "unsupported-name", "{id}");
        assert_eq!(refusal["kind"], "native-limit", "{id}");
    }
    // Dot ids have escaped directory names, so they reach normal lookup.
    for id in [".", ".."] {
        let refusal = root_refusal(&lookup_in_with_id(
            &scratch.0,
            &home,
            "missing".as_ref(),
            id,
        ));
        assert_eq!(refusal["reason"], "not-found", "{id}");
    }
}

#[cfg(windows)]
#[test]
fn windows_layout_refuses_literal_trailing_dots_and_spaces() {
    for relative in [
        "project.",
        "project ",
        "project/session.",
        "project/session ",
    ] {
        let scratch = Scratch::new("windows-listed-names");
        let home = scratch.0.join("home");
        std::fs::create_dir(&home).unwrap();
        // An extended path creates the literal name Node would see; the CLI
        // receives an ordinary root and must refuse before joining that name.
        let extended = std::fs::canonicalize(&scratch.0).unwrap();
        std::fs::create_dir_all(extended.join("root").join(relative)).unwrap();
        let refusal = root_refusal(&lookup_in(&scratch.0, &home, "root".as_ref()));
        assert_eq!(refusal["stage"], "layout", "{relative}");
        assert_eq!(refusal["reason"], "unsupported-name", "{relative}");
        assert_eq!(refusal["kind"], "native-limit", "{relative}");
    }
}
