//! Runs the built `bake-session-lease-probe` as independent processes over a
//! private Session root per test and checks that one process's write handle
//! excludes another's until it is released or the process is killed. The
//! cross-runtime suite pits the probe against TypeScript; these tests only
//! check the probe's own protocol against itself.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};

/// A bound on every wait for a probe's output, not a readiness delay.
const DEADLINE: Duration = Duration::from_secs(60);
/// A bound on the stdout lines and bytes kept from one probe.
const MAX_LINES: usize = 8;
const MAX_LINE_BYTES: u64 = 4096;

const OWNED: &str = "session \"s\" is already owned by an active write handle";
const HEADER: &str = "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"createdAt\":1000,\"isSeeded\":false,\"delegationDepth\":0}\n";
const SEQ0: &str = "{\"type\":\"turn/start\",\"seq\":0,\"time\":1,\"data\":{\"turn\":1}}\n";
const SEQ1: &str = "{\"type\":\"turn/end\",\"seq\":1,\"time\":2,\"data\":{\"turn\":1,\"reason\":{\"kind\":\"completed\"}}}\n";
const SEQ2: &str = "{\"type\":\"turn/start\",\"seq\":2,\"time\":3,\"data\":{\"turn\":2}}\n";
/// A released v2 Session `v2v3` and the v3 log TypeScript migrates it to,
/// from `conformance/session/fault-cases.json`.
const V2_SOURCE: &str = "{\"type\":\"session\",\"version\":2,\"id\":\"v2v3\",\"createdAt\":1700000000000,\"isSeeded\":true,\"delegationDepth\":0,\"parentSession\":\"parent\"}\n{\"type\":\"session/end-seed\",\"seq\":0,\"time\":1000,\"data\":{\"inherited\":true}}\n{\"type\":\"turn/start\",\"seq\":1,\"time\":1001,\"data\":{\"turn\":1}}\n{\"type\":\"step/start\",\"seq\":2,\"time\":1002,\"data\":{\"turn\":1,\"step\":1}}\n";
const V2_MIGRATED: &str = "{\"type\":\"session\",\"version\":3,\"id\":\"v2v3\",\"createdAt\":1700000000000,\"parentSession\":\"parent\",\"isSeeded\":true,\"delegationDepth\":0}\n{\"type\":\"session/end-seed\",\"seq\":0,\"time\":1000,\"data\":{\"inherited\":true}}\n{\"type\":\"turn/start\",\"seq\":1,\"time\":1001,\"data\":{\"turn\":1}}\n{\"type\":\"step/start\",\"seq\":2,\"time\":1002,\"data\":{\"turn\":1,\"step\":1}}\n{\"type\":\"system/message\",\"seq\":3,\"time\":1002,\"data\":{\"turn\":1,\"step\":1,\"message\":{\"id\":\"v2-to-v3-system-e12c67eac0ceb4f8c38adb1b85eab50b81369336240a868e62caaccf738872f7\",\"role\":\"system\",\"source\":{\"kind\":\"plugin\",\"plugin\":\"@deepseek-ai/dsh-system-prompt\"},\"content\":[]}},\"surfaceOp\":\"append\"}\n";
const V2_SEQ4: &str = "{\"type\":\"turn/start\",\"seq\":4,\"time\":3,\"data\":{\"turn\":2}}\n";

/// A private directory removed on drop; exclusive creation retries on a stale
/// name. Declare it before any probe so every probe is reaped first.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bake-lease-probe-{}-{name}-{n}",
                std::process::id()
            ));
            #[allow(unused_mut)]
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot create temp root: {error}"),
            }
        }
    }

    fn arg(&self) -> &str {
        self.0.to_str().expect("temp root is UTF-8")
    }

    fn log(&self) -> PathBuf {
        self.0.join("_no-cwd").join("s").join("session.v3.jsonl")
    }

    fn log_text(&self) -> String {
        fs::read_to_string(self.log()).expect("the log is readable")
    }

    /// The Session directory of `v2v3`, seeded with its v2 source.
    fn seed_v2(&self) -> PathBuf {
        let dir = self.0.join("_no-cwd").join("v2v3");
        fs::create_dir_all(&dir).expect("the Session directory is created");
        fs::write(dir.join("session.v2.jsonl"), V2_SOURCE).expect("the source is seeded");
        dir
    }
}

/// The names `dir` lists, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One probe process. Its stdout lines arrive over a channel, `None` at EOF,
/// so every wait is bounded; dropping an unfinished probe kills and reaps it
/// and joins its reader threads.
struct Probe {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Option<String>>,
    readers: Vec<JoinHandle<()>>,
    stderr: Receiver<String>,
    finished: bool,
}

impl Probe {
    fn spawn(args: &[&str], stdin: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake-session-lease-probe"));
        command
            .args(args)
            .env_clear()
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let mut child = command.spawn().expect("probe starts");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (line_tx, lines) = mpsc::channel();
        let (err_tx, stderr_rx) = mpsc::channel();
        let out_reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            for _ in 0..MAX_LINES {
                let mut line = String::new();
                match reader.by_ref().take(MAX_LINE_BYTES).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if line_tx.send(Some(line)).is_err() {
                            return;
                        }
                    }
                }
            }
            // Drain anything past the bound so the probe never blocks on a
            // full pipe; the test sees EOF only after the pipe closes.
            let _ = io::copy(&mut reader, &mut io::sink());
            let _ = line_tx.send(None);
        });
        let err_reader = thread::spawn(move || {
            let mut text = Vec::new();
            let mut stderr = stderr;
            let _ = stderr.by_ref().take(64 * 1024).read_to_end(&mut text);
            let _ = io::copy(&mut stderr, &mut io::sink());
            let _ = err_tx.send(String::from_utf8_lossy(&text).into_owned());
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            readers: vec![out_reader, err_reader],
            stderr: stderr_rx,
            finished: false,
        }
    }

    /// The next stdout line, or `None` at EOF.
    fn line(&self) -> Option<String> {
        match self.lines.recv_timeout(DEADLINE) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("probe output timed out"),
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn json_line(&self) -> Value {
        let line = self.line().expect("probe prints a line");
        let text = line.strip_suffix('\n').expect("the line ends in LF");
        serde_json::from_str(text).expect("the line is JSON")
    }

    /// Await stdout EOF, then the exit; returns the status and stderr.
    fn finish(&mut self) -> (ExitStatus, String) {
        self.stdin = None;
        assert_eq!(self.line(), None, "no stdout after the last expected line");
        let status = self.child.wait().expect("probe is awaited");
        self.finished = true;
        for reader in self.readers.drain(..) {
            reader.join().expect("reader thread");
        }
        let stderr = self
            .stderr
            .recv_timeout(DEADLINE)
            .expect("stderr is collected");
        (status, stderr)
    }

    /// Kill the probe, which bypasses its `Drop`, and reap it.
    fn kill(&mut self) {
        self.child.kill().expect("probe is killed");
        let (status, _) = self.finish();
        assert!(!status.success(), "a killed probe does not exit 0");
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(9),
            "the probe died of SIGKILL"
        );
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("piped stdin");
        stdin.write_all(bytes).expect("stdin accepts the command");
        stdin.flush().expect("stdin flushes");
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        if !self.finished {
            self.stdin = None;
            let _ = self.child.kill();
            let _ = self.child.wait();
            for reader in self.readers.drain(..) {
                let _ = reader.join();
            }
        }
    }
}

/// Start a holder and await its readiness line.
fn holder(mode: &str, root: &TempRoot) -> Probe {
    let probe = Probe::spawn(&[mode, root.arg(), "s"], true);
    assert_eq!(probe.json_line(), json!({ "state": "holding" }));
    probe
}

/// The exit code, stdout lines, and stderr of a one-shot probe.
fn run(args: &[&str]) -> (Option<i32>, Vec<String>, String) {
    let mut probe = Probe::spawn(args, false);
    let mut lines = Vec::new();
    while let Some(line) = probe.line() {
        lines.push(line);
    }
    let (status, stderr) = probe.finish();
    (status.code(), lines, stderr)
}

/// A contender's `open` is refused as owned and leaves the log unchanged.
fn assert_refused(root: &TempRoot) {
    let before = fs::read(root.log()).expect("the log exists");
    let (code, lines, stderr) = run(&["open", root.arg(), "s", "2"]);
    assert_eq!(code, Some(3), "stderr: {stderr}");
    assert_eq!(
        lines,
        [format!(
            "{}\n",
            json!({ "outcome": "owned", "message": OWNED })
        )]
    );
    assert_eq!(stderr, "");
    assert_eq!(fs::read(root.log()).unwrap(), before, "refused open wrote");
}

/// A contender's `open` appends seq 2 and reports the stored seqs.
fn assert_takes_over(root: &TempRoot) {
    let (code, lines, stderr) = run(&["open", root.arg(), "s", "2"]);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(
        lines,
        [format!(
            "{}\n",
            json!({ "outcome": "opened", "seqs": [0, 1, 2] })
        )]
    );
    assert_eq!(root.log_text(), [HEADER, SEQ0, SEQ1, SEQ2].concat());
}

#[test]
fn live_holder_excludes_until_graceful_release() {
    let root = TempRoot::new("release");
    let mut holder = holder("hold", &root);
    assert_eq!(root.log_text(), [HEADER, SEQ0, SEQ1].concat());
    assert_refused(&root);
    holder.send(b"release\n");
    assert_eq!(holder.json_line(), json!({ "state": "released" }));
    let (status, stderr) = holder.finish();
    assert!(status.success(), "stderr: {stderr}");
    assert_takes_over(&root);
}

#[test]
fn killed_holder_permits_takeover() {
    let root = TempRoot::new("kill");
    let mut holder = holder("hold", &root);
    assert_refused(&root);
    holder.kill();
    assert_takes_over(&root);
}

#[test]
fn hold_open_excludes_until_killed() {
    let root = TempRoot::new("hold-open");
    let mut creator = holder("hold", &root);
    // Stdin EOF releases silently.
    let (status, stderr) = creator.finish();
    assert!(status.success(), "stderr: {stderr}");
    let mut holder = holder("hold-open", &root);
    assert_eq!(root.log_text(), [HEADER, SEQ0, SEQ1].concat());
    assert_refused(&root);
    holder.kill();
    assert_takes_over(&root);
}

#[test]
fn other_refusals_are_not_ownership() {
    let root = TempRoot::new("refusals");
    let (code, lines, stderr) = run(&["open", root.arg(), "s", "0"]);
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert_eq!(
        lines,
        [format!(
            "{}\n",
            json!({ "outcome": "refused", "message": "session \"s\" not found" })
        )]
    );
    assert_eq!(stderr, "");

    let mut creator = holder("hold", &root);
    let (status, _) = creator.finish();
    assert!(status.success());
    let before = root.log_text();
    let mut duplicate = Probe::spawn(&["hold", root.arg(), "s"], true);
    let (status, stderr) = duplicate.finish();
    assert_eq!(status.code(), Some(1));
    assert!(stderr.contains("already exists"), "stderr: {stderr}");

    let (code, lines, stderr) = run(&["open", root.arg(), "s", "5"]);
    assert_eq!((code, lines.len()), (Some(1), 0));
    assert!(stderr.contains("seq mismatch"), "stderr: {stderr}");
    assert_eq!(root.log_text(), before, "a refused append wrote");
    assert_takes_over(&root);
}

#[test]
fn unexpected_holder_command_is_a_fixture_error() {
    let root = TempRoot::new("command");
    let mut holder = holder("hold", &root);
    holder.send(b"stop\n");
    let (status, stderr) = holder.finish();
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("unexpected holder command"), "{stderr}");
    assert_takes_over(&root);
}

#[test]
fn wrong_arguments_exit_2_without_stdout() {
    let root = TempRoot::new("usage");
    let relative = Path::new("relative").to_str().unwrap();
    let cases: &[&[&str]] = &[
        &[],
        &["hold"],
        &["hold", root.arg()],
        &["hold", root.arg(), "s", "extra"],
        &["hold", relative, "s"],
        &["hold", root.arg(), ""],
        &["hold-open", relative, "s"],
        &["open", root.arg(), "s"],
        &["open", relative, "s", "2"],
        &["open", root.arg(), "s", ""],
        &["open", root.arg(), "s", "+2"],
        &["open", root.arg(), "s", "-1"],
        &["open", root.arg(), "s", "2.0"],
        &["open", root.arg(), "s", "9007199254740992"],
        &["release", root.arg(), "s"],
        &["tear-create", root.arg(), "s"],
        &["tear-create", root.arg(), "s", "0"],
        &["tear-create", relative, "s", "1"],
        &["tear-append", root.arg(), "s", "2"],
        &["tear-append", root.arg(), "s", "2", "0"],
        &["tear-append", root.arg(), "s", "9007199254740991", "1"],
        &["tear-append", relative, "s", "2", "1"],
    ];
    for args in cases {
        let (code, lines, stderr) = run(args);
        assert_eq!(code, Some(2), "{args:?}");
        assert!(lines.is_empty(), "{args:?} printed {lines:?}");
        assert!(stderr.starts_with("bake-session-lease-probe: "), "{args:?}");
    }
    let entries = fs::read_dir(&root.0).unwrap().count();
    assert_eq!(entries, 0, "a usage error wrote under the root");
}

/// Await a tearing writer's `torn` line; it then holds the lock until killed.
fn torn(args: &[&str]) -> Probe {
    let probe = Probe::spawn(args, true);
    assert_eq!(probe.json_line(), json!({ "state": "torn" }));
    probe
}

#[test]
fn tear_append_stores_its_budget_and_holds_the_lock_until_killed() {
    let root = TempRoot::new("tear-append");
    let mut creator = holder("hold", &root);
    let (status, _) = creator.finish();
    assert!(status.success());
    let seq3 = "{\"type\":\"turn/end\",\"seq\":3,\"time\":4,\"data\":{\"turn\":2,\"reason\":{\"kind\":\"completed\"}}}\n";
    let budget = SEQ2.len() + 5;
    let mut writer = torn(&["tear-append", root.arg(), "s", "2", &budget.to_string()]);
    assert_eq!(
        root.log_text(),
        [HEADER, SEQ0, SEQ1, SEQ2, &seq3[..5]].concat()
    );
    assert_refused(&root);
    writer.kill();
    // The complete row is kept and the fragment truncated before seq 3.
    let (code, lines, stderr) = run(&["open", root.arg(), "s", "3"]);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(
        lines,
        [format!(
            "{}\n",
            json!({ "outcome": "opened", "seqs": [0, 1, 2, 3] })
        )]
    );
}

#[test]
fn tear_create_leaves_only_its_temporary_file() {
    let root = TempRoot::new("tear-create");
    let mut writer = torn(&["tear-create", root.arg(), "s", "30"]);
    let dir = root.0.join("_no-cwd").join("s");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(names[0], "session.lock");
    let staged = &names[1];
    assert!(
        staged.starts_with("session.v3.jsonl.") && staged.ends_with(".tmp"),
        "{staged}"
    );
    assert_eq!(fs::read_to_string(dir.join(staged)).unwrap(), HEADER[..30]);
    writer.kill();
    let (code, lines, _) = run(&["open", root.arg(), "s", "0"]);
    assert_eq!(code, Some(1));
    assert_eq!(
        lines,
        [format!(
            "{}\n",
            json!({ "outcome": "refused", "message": "session \"s\" not found" })
        )]
    );
}

#[test]
fn writes_within_the_tear_budget_are_a_fixture_error() {
    let root = TempRoot::new("untorn");
    let (code, lines, stderr) = run(&["tear-create", root.arg(), "s", "100000"]);
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(lines.is_empty());
    assert!(stderr.contains("within the tear budget"), "{stderr}");
}

#[test]
fn pause_migrate_holds_the_lock_before_publishing_until_resumed() {
    let root = TempRoot::new("pause-publish");
    let dir = root.seed_v2();
    let mut paused = Probe::spawn(&["pause-migrate", root.arg(), "v2v3", "publish", "4"], true);
    assert_eq!(paused.json_line(), json!({ "state": "paused" }));
    assert_eq!(names(&dir), ["session.lock", "session.v2.jsonl"]);
    let (code, lines, _) = run(&["open", root.arg(), "v2v3", "4"]);
    assert_eq!(code, Some(3));
    assert_eq!(lines.len(), 1);
    paused.send(b"go\n");
    assert_eq!(
        paused.json_line(),
        json!({ "outcome": "opened", "seqs": [0, 1, 2, 3, 4] })
    );
    let (status, stderr) = paused.finish();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        fs::read_to_string(dir.join("session.v3.jsonl")).unwrap(),
        [V2_MIGRATED, V2_SEQ4].concat()
    );
    assert_eq!(
        fs::read_to_string(dir.join("session.v2.jsonl")).unwrap(),
        V2_SOURCE
    );
    assert_eq!(
        names(&dir),
        ["session.lock", "session.v2.jsonl", "session.v3.jsonl"]
    );
}

#[cfg(unix)]
#[test]
fn pause_migrate_unlink_stops_after_the_link_and_exits_at_eof() {
    let root = TempRoot::new("pause-unlink");
    let dir = root.seed_v2();
    let mut paused = Probe::spawn(&["pause-migrate", root.arg(), "v2v3", "unlink", "4"], true);
    assert_eq!(paused.json_line(), json!({ "state": "paused" }));
    let listed = names(&dir);
    assert_eq!(listed.len(), 4, "{listed:?}");
    // `session.lock` sorts before `session.migration.*`.
    let staged = &listed[1];
    assert!(
        staged.starts_with("session.migration.") && staged.ends_with(".jsonl.tmp"),
        "{listed:?}"
    );
    assert_eq!(fs::read_to_string(dir.join(staged)).unwrap(), V2_MIGRATED);
    assert_eq!(
        fs::read_to_string(dir.join("session.v3.jsonl")).unwrap(),
        V2_MIGRATED
    );
    // Stdin EOF exits without resuming, so the temporary file stays.
    let (status, stderr) = paused.finish();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(names(&dir), listed);
}

#[test]
fn pause_migrate_without_a_migration_or_with_an_unknown_step_is_a_fixture_error() {
    let root = TempRoot::new("pause-usage");
    let mut creator = holder("hold", &root);
    let (status, _) = creator.finish();
    assert!(status.success());
    let (code, lines, stderr) = run(&["pause-migrate", root.arg(), "s", "publish", "2"]);
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(lines.is_empty());
    assert!(stderr.contains("reached no migration pause"), "{stderr}");
    let (code, lines, stderr) = run(&["pause-migrate", root.arg(), "s", "later", "2"]);
    assert_eq!((code, lines.len()), (Some(2), 0), "stderr: {stderr}");
    assert_eq!(root.log_text(), [HEADER, SEQ0, SEQ1].concat());
}
