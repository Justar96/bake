//! Runs the built `bake-conformance-runner` against the shared fixtures in a
//! private workspace per case and checks its exit code, streams, and the
//! workspace's final regular files.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

/// Creates the directory atomically with mode 0700 on Unix; Windows keeps
/// its default ACL.
fn private_dir_builder() -> fs::DirBuilder {
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

/// A private directory removed on drop; exclusive creation retries on a stale name.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bake-conformance-process-{}-{name}-{n}",
                std::process::id()
            ));
            match private_dir_builder().create(&path) {
                Ok(()) => return TempRoot(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create temp root: {error}"),
            }
        }
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn conformance_files(kind: &str) -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../conformance")
        .join(kind);
    let mut files: Vec<_> = fs::read_dir(dir)
        .expect("shared conformance directory exists")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            (stem, fs::read(&path).expect("fixture is readable"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no {kind} fixtures found");
    files
}

/// Runs the binary in `cwd` with an empty environment, so the result cannot
/// depend on the test host's variables. The child is always awaited, and is
/// killed first if stdin fails for any reason other than an early exit.
fn run(cwd: &Path, stdin: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bake-conformance-runner"));
    command
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    let mut child = command.spawn().expect("runner starts");
    let mut pipe = child.stdin.take().unwrap();
    // An oversized input makes the runner stop reading and exit early.
    match pipe.write_all(stdin) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        Err(error) => {
            drop(pipe);
            let _ = child.kill();
            let _ = child.wait();
            panic!("cannot write runner stdin: {error}");
        }
    }
    drop(pipe);
    child.wait_with_output().expect("runner is awaited")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("fixture hex"))
        .collect()
}

/// Every regular file under `root` by slash path; anything else fails the test.
fn regular_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                walk(root, &path, files);
            } else if kind.is_file() {
                let relative = path.strip_prefix(root).unwrap().components();
                let name: Vec<_> = relative
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                files.insert(name.join("/"), fs::read(&path).unwrap());
            } else {
                panic!("unexpected non-regular file {}", path.display());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

fn file_map(entries: &Value) -> BTreeMap<String, Vec<u8>> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().to_owned(),
                hex(file["hex"].as_str().unwrap()),
            )
        })
        .collect()
}

fn assert_rejected(output: &Output, code: i32, what: &str) -> String {
    assert_eq!(output.status.code(), Some(code), "{what}: {output:?}");
    assert!(
        output.stdout.is_empty(),
        "{what}: stdout {:?}",
        output.stdout
    );
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert!(
        stderr.starts_with("bake-conformance-runner: "),
        "{what}: {stderr}"
    );
    assert_eq!(stderr.matches('\n').count(), 1, "{what}: {stderr}");
    stderr
}

#[test]
fn fixtures_produce_their_expected_observation_and_files() {
    for (id, bytes) in conformance_files("fixtures") {
        let fixture: Value = serde_json::from_slice(&bytes).unwrap();
        let root = TempRoot::new(&id);
        let initial = file_map(&fixture["initialFiles"]);
        for (path, contents) in &initial {
            let target = root.0.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, contents).unwrap();
        }

        // The child receives only the input; expected values stay here.
        let output = run(&root.0, &serde_json::to_vec(&fixture["input"]).unwrap());
        assert_eq!(output.status.code(), Some(0), "{id}: {output:?}");
        assert!(output.stderr.is_empty(), "{id}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.ends_with('\n') && stdout.matches('\n').count() == 1,
            "{id}: {stdout:?}"
        );

        let expected = &fixture["expected"];
        let observed: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            observed,
            json!({
                "schema": "bake/synthetic-conformance/observation",
                "version": 1,
                "prompts": expected["prompts"],
                "events": expected["events"],
                "permissions": expected["permissions"],
            }),
            "{id}"
        );
        let files = regular_files(&root.0);
        assert_eq!(files, file_map(&expected["files"]), "{id}");
        for path in fixture["protectedFiles"].as_array().unwrap() {
            let path = path.as_str().unwrap();
            assert_eq!(files.get(path), initial.get(path), "{id}: protected {path}");
        }
    }
}

#[test]
fn denied_writes_leave_existing_and_absent_files_untouched() {
    let root = TempRoot::new("deny");
    fs::write(root.0.join("kept.txt"), b"original\n").unwrap();
    let input = br#"{"schema":"bake/synthetic-conformance/input","version":1,"prompts":[],"events":[],
        "permissions":[{"id":"e","path":"kept.txt","decision":"deny"},{"id":"c","path":"new/created.txt","decision":"deny"}],
        "writes":[{"path":"kept.txt","text":"changed","permission":"e"},{"path":"new/created.txt","text":"x","permission":"c"}]}"#;
    let output = run(&root.0, input);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        regular_files(&root.0),
        BTreeMap::from([("kept.txt".to_owned(), b"original\n".to_vec())])
    );
    assert!(
        !root.0.join("new").exists(),
        "a denied write created its parent"
    );
}

#[test]
fn shared_invalid_inputs_exit_2_without_side_effects() {
    let reasons = [
        ("duplicate-permission", "duplicates an earlier permission"),
        ("future-version", "version must be 1"),
        ("non-integer", "not a safe integer"),
        ("unknown-field", "unknown field `unexpected`"),
        ("unknown-permission", "names no permission"),
        ("unpaired-surrogate", "hex escape"),
        ("unsafe-path", "dot segment"),
    ];
    for (name, bytes) in conformance_files("invalid") {
        let root = TempRoot::new(&name);
        let stderr = assert_rejected(&run(&root.0, &bytes), 2, &name);
        if let Some((_, reason)) = reasons.iter().find(|(known, _)| *known == name) {
            assert!(stderr.contains(reason), "{name}: {stderr}");
        }
        assert!(regular_files(&root.0).is_empty(), "{name}");
    }
}

#[test]
fn invalid_input_after_a_valid_allowed_write_writes_nothing() {
    let root = TempRoot::new("validate-first");
    let input = br#"{"schema":"bake/synthetic-conformance/input","version":1,"prompts":[],"events":[{"n":1}],
        "permissions":[{"id":"p","path":"first.txt","decision":"allow"},{"id":"q","path":"CON.txt","decision":"allow"}],
        "writes":[{"path":"first.txt","text":"x","permission":"p"},{"path":"CON.txt","text":"x","permission":"q"}]}"#;
    let stderr = assert_rejected(&run(&root.0, input), 2, "reserved name");
    assert!(stderr.contains("reserved device name"), "{stderr}");
    assert!(regular_files(&root.0).is_empty());
}

#[test]
fn empty_and_oversized_stdin_exit_2() {
    let root = TempRoot::new("bounds");
    assert_rejected(&run(&root.0, b""), 2, "empty");
    let stderr = assert_rejected(&run(&root.0, &vec![b' '; 256 * 1024 + 1]), 2, "oversized");
    assert!(stderr.contains("exceeds 262144 bytes"), "{stderr}");
}

#[test]
fn write_failure_exits_1_without_an_observation() {
    let root = TempRoot::new("io-failure");
    fs::create_dir(root.0.join("taken")).unwrap();
    let input =
        br#"{"schema":"bake/synthetic-conformance/input","version":1,"prompts":[],"events":[],
        "permissions":[{"id":"p","path":"taken","decision":"allow"}],
        "writes":[{"path":"taken","text":"x","permission":"p"}]}"#;
    let stderr = assert_rejected(&run(&root.0, input), 1, "directory target");
    assert!(stderr.contains("I/O failure"), "{stderr}");
}
