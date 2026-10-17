//! `bake-rs --self-check` run as the packager runs it: piped streams, a
//! cleared environment, and a PATH with no Node or Bun.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use ring::digest::{SHA256, digest};
use serde_json::Value;

/// A directory this test creates, whose name holds a space and non-ASCII
/// text, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "bake-rs self-check ñ {name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        // Exclusive: a leftover directory of the same name is a failure, not reused.
        fs::create_dir(&path).expect("a fresh scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `bake-rs` with only the variables the arguments set, plus what Windows
/// needs to start a process at all.
fn command(home: &Path, path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
    command.env_clear();
    for name in ["SYSTEMROOT", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("PATH", path)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("BAKE_HOME", home.join(".bake"))
        .env("DSH_HOME", home.join(".dsh"))
        .stdin(Stdio::null());
    command
}

fn self_check(home: &Path, path: &Path, extra: &[&str]) -> Output {
    command(home, path)
        .arg("--self-check")
        .args(extra)
        .output()
        .expect("bake-rs runs")
}

/// Every path under `root` with its bytes and modification time.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, Option<SystemTime>)> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, Option<SystemTime>)>) {
        for entry in fs::read_dir(dir).expect("readable") {
            let path = entry.expect("entry").path();
            let meta = fs::symlink_metadata(&path).expect("metadata");
            let modified = meta.modified().ok();
            if meta.is_dir() {
                out.insert(path.clone(), (Vec::new(), modified));
                walk(&path, out);
            } else {
                out.insert(path.clone(), (fs::read(&path).expect("bytes"), modified));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &mut out);
    out
}

/// A user home whose Bake 0.4 and 0.3 settings and credentials are all
/// unreadable as what they claim to be.
fn broken_home(root: &Path) {
    for dir in [".bake", ".dsh"] {
        fs::create_dir(root.join(dir)).expect("home");
        for (file, text) in [
            ("settings.json", "{ not json"),
            ("auth.json", "[\"not an object\""),
            ("models.json", "\u{0}\u{1}"),
            ("settings.yaml", "providers: [unclosed"),
            (".credentials.yaml", "key: *missing-anchor"),
        ] {
            fs::write(root.join(dir).join(file), text).expect("seed");
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn the_record_names_this_build_and_its_documentation_sources() {
    let home = Scratch::new("home");
    let path = Scratch::new("path");
    broken_home(&home.0);
    let before = snapshot(&home.0);
    let out = self_check(&home.0, &path.0, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    assert_eq!(snapshot(&home.0), before, "the self-check touched the home");
    assert!(snapshot(&path.0).is_empty(), "the self-check wrote to PATH");

    let line = String::from_utf8(out.stdout).expect("UTF-8");
    assert!(
        line.ends_with('\n') && line.matches('\n').count() == 1,
        "{line:?}"
    );
    let record: Value = serde_json::from_str(&line).expect("JSON");
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("windows", "x86_64") => "win32-x64",
        other => panic!("{other:?} is not a release platform; the record must fail"),
    };
    // The documentation sources, read here rather than through the crate's
    // topic list, so a topic left out of the binary is caught.
    let mut docs = serde_json::Map::new();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    for entry in fs::read_dir(&dir).expect("docs") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|ext| ext == "md") {
            let name = path.file_stem().and_then(|s| s.to_str()).expect("name");
            let bytes = fs::read(&path).expect("doc");
            docs.insert(
                name.to_owned(),
                hex(digest(&SHA256, &bytes).as_ref()).into(),
            );
        }
    }
    assert!(docs.len() >= 8);
    assert_eq!(
        record,
        serde_json::json!({
            "kind": "bake-rs-self-check",
            "schema": 1,
            "version": env!("CARGO_PKG_VERSION"),
            "runtime": "rust",
            "target": target,
            "docs": docs,
        })
    );
}

#[test]
fn extra_arguments_are_refused_with_usage_guidance() {
    let home = Scratch::new("home");
    let path = Scratch::new("path");
    for extra in [&["--json"][..], &["now"], &["--self-check"], &["--help"]] {
        let out = self_check(&home.0, &path.0, extra);
        assert_eq!(out.status.code(), Some(2), "{extra:?}");
        assert!(out.stdout.is_empty(), "{extra:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!(
                "unexpected argument '{}' for '--self-check'",
                extra[0]
            )),
            "{stderr}"
        );
        assert!(stderr.contains("bake-rs --help"), "{stderr}");
    }
    assert!(
        snapshot(&home.0).is_empty(),
        "a refused self-check wrote to the home"
    );
}

/// Standard output is a pipe whose reader is closed before the child starts,
/// so the record's write fails with a broken pipe: status 1 and no panic.
#[test]
fn a_closed_standard_output_fails_without_a_panic() {
    let home = Scratch::new("home");
    let path = Scratch::new("path");
    let (reader, writer) = std::io::pipe().expect("a pipe");
    drop(reader);
    let out = command(&home.0, &path.0)
        .arg("--self-check")
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("bake-rs runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");
}
