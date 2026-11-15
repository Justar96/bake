//! The locked JSON store (`bake_session::json_store`) under updates from
//! independent processes, through the credential and settings stores built
//! on it.
//!
//! The tests run this test binary again, filtered to [`child_process`],
//! with its role in the child's own environment. Children report readiness
//! on stdout and wait for a line on stdin, so the parent releases them
//! together without sleeping. Every wait on a child is bounded, and a child
//! is killed, reaped, and its output reader joined when a test fails.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bake_ai::ModelThinkingLevel;
use serde_json::{Map, Value, json};

use crate::auth_storage::{AuthStorage, Credential};
use crate::json_store::{JsonFileStore, StoreError, set_member};
use crate::settings::{Settings, SettingsStore};
use crate::test_support::TempDir;

const LIMIT: u64 = 1024;

/// The directory's entries, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read")).expect("json")
}

/// Sets `a` to 1; the object as stored, for an update that was flushed.
fn set_a(store: &JsonFileStore) -> Result<Map<String, Value>, StoreError> {
    let updated = store.update(|object| set_member(object, "a", json!(1)))?;
    assert!(updated.unsynced.is_none(), "{:?}", updated.unsynced);
    Ok(updated.object)
}

const CHILD_ROLE: &str = "BAKE_JSON_STORE_TEST_ROLE";
const CHILD_HOME: &str = "BAKE_JSON_STORE_TEST_HOME";
const CHILD_INDEX: &str = "BAKE_JSON_STORE_TEST_INDEX";
const READY: &str = "bake-json-store-child-ready";
const WRITES_PER_CHILD: usize = 15;

/// How long a child may take to report ready, or to exit once released.
const CHILD_BOUND: Duration = Duration::from_secs(60);

/// Waits for the parent's go-ahead line.
fn wait_for_parent() {
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
}

/// A child of the cross-process tests; does nothing in an ordinary run.
#[test]
#[ignore = "run by the cross-process tests in a child process"]
fn child_process() {
    let (Ok(role), Ok(home)) = (std::env::var(CHILD_ROLE), std::env::var(CHILD_HOME)) else {
        return;
    };
    let home = PathBuf::from(home);
    let index = std::env::var(CHILD_INDEX).expect("index");
    match role.as_str() {
        "writer" => {
            // Loaded before the others write: stale for every write below.
            let mut auth = AuthStorage::load(&home.join("auth.json"));
            let settings = SettingsStore::new(&home);
            println!("{READY}");
            wait_for_parent();
            for write in 0..WRITES_PER_CHILD {
                auth.set_api_key(
                    &format!("child{index}-{write}"),
                    &format!("secret-{index}-{write}"),
                )
                .expect("auth write");
                settings
                    .set_model_thinking_level(
                        &format!("child{index}"),
                        &format!("model{write}"),
                        ModelThinkingLevel::Low,
                    )
                    .expect("settings write");
            }
        }
        "holder" => {
            let store = JsonFileStore::new(home.join("auth.json"), LIMIT);
            let _lock = store.lock().expect("lock");
            println!("{READY}");
            wait_for_parent();
        }
        // Never reports ready: the parent's bounded wait must end it.
        "silent" => wait_for_parent(),
        // Exits without reporting ready.
        "quits" => {}
        other => panic!("unknown role {other}"),
    }
}

/// What the output reader saw.
enum Output {
    Ready,
    Closed,
}

/// A child process re-running this test binary. Dropped while running, a
/// failed test included, it is killed, reaped within [`CHILD_BOUND`], and its
/// output reader joined.
struct ChildProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    output: Receiver<Output>,
    reader: Option<JoinHandle<()>>,
}

impl ChildProcess {
    fn spawn(role: &str, home: &Path, index: usize) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "json_store_process_tests::child_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ROLE, role)
            .env(CHILD_HOME, home)
            .env(CHILD_INDEX, index.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn child");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (send, output) = mpsc::channel();
        // Drains the child's output until it closes, so the child never
        // writes to a closed pipe; libtest may print the marker after a
        // test's name on one line.
        let reader = std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if line.contains(READY) => {
                            let _ = send.send(Output::Ready);
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
            let _ = send.send(Output::Closed);
        });
        Self {
            child,
            stdin,
            output,
            reader: Some(reader),
        }
    }

    /// Waits up to `within` for the readiness line.
    fn wait_ready(&mut self, within: Duration) -> Result<(), String> {
        match self.output.recv_timeout(within) {
            Ok(Output::Ready) => Ok(()),
            Ok(Output::Closed) | Err(RecvTimeoutError::Disconnected) => {
                Err("the child closed its output before it was ready".to_owned())
            }
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "the child did not report ready within {} ms",
                within.as_millis()
            )),
        }
    }

    fn release(&mut self) {
        let mut stdin = self.stdin.take().expect("stdin");
        stdin.write_all(b"go\n").expect("release child");
    }

    /// Polls for the child's exit until `within` has passed; once it has
    /// exited, joins the output reader, whose pipe the exit closed.
    fn wait_exit(&mut self, within: Duration) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + within;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    if let Some(reader) = self.reader.take() {
                        reader
                            .join()
                            .map_err(|_| "the output reader panicked".to_owned())?;
                    }
                    return Ok(status);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(None) => {
                    return Err(format!(
                        "the child did not exit within {} ms",
                        within.as_millis()
                    ));
                }
                Err(error) => return Err(format!("could not wait for the child: {error}")),
            }
        }
    }

    /// Kills the child and reaps it.
    fn kill(&mut self) -> ExitStatus {
        // Fails only when the child has already exited.
        let _ = self.child.kill();
        self.wait_exit(CHILD_BOUND)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// Waits for a released child to exit successfully.
    fn finish(mut self) {
        let status = self
            .wait_exit(CHILD_BOUND)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(status.success(), "child failed: {status}");
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if self.reader.is_none() {
            return;
        }
        drop(self.stdin.take());
        let _ = self.child.kill();
        // A reader whose child could not be reaped is left detached rather
        // than joined forever.
        let reaped = self.wait_exit(CHILD_BOUND);
        if !std::thread::panicking() {
            reaped.unwrap_or_else(|error| panic!("{error}"));
        }
    }
}

#[test]
fn a_child_that_never_reports_ready_is_bounded_and_cleaned_up() {
    let dir = TempDir::new("json-store-silent");
    let mut silent = ChildProcess::spawn("silent", dir.path(), 0);
    let error = silent
        .wait_ready(Duration::from_millis(200))
        .expect_err("silent");
    assert_eq!(error, "the child did not report ready within 200 ms");
    assert!(!silent.kill().success());
    assert!(silent.reader.is_none(), "the output reader was joined");

    let mut quits = ChildProcess::spawn("quits", dir.path(), 0);
    assert_eq!(
        quits.wait_ready(CHILD_BOUND),
        Err("the child closed its output before it was ready".to_owned())
    );
    quits.finish();
}

#[test]
fn independent_processes_with_stale_copies_lose_no_update() {
    const CHILDREN: usize = 4;
    let dir = TempDir::new("json-store-processes");
    let home = dir.join("home");
    fs::create_dir(&home).expect("home");
    let oauth = json!({
        "type": "oauth", "access": "access-token", "refresh": "refresh-token",
        "expires": 1_700_000_000_000u64, "accountId": "acct", "scopes": ["a", "b"],
    });
    let unknown = json!({ "type": "future", "blob": { "n": 1 } });
    fs::write(
        home.join("auth.json"),
        serde_json::to_string(&json!({ "oauth-provider": oauth, "future-provider": unknown }))
            .expect("auth"),
    )
    .expect("write auth");
    fs::write(
        home.join("settings.json"),
        r#"{"theme":"dark","packages":["npm:x"],"modelThinkingLevels":{"keep/me":"high"}}"#,
    )
    .expect("write settings");

    let mut children: Vec<ChildProcess> = (0..CHILDREN)
        .map(|index| ChildProcess::spawn("writer", &home, index))
        .collect();
    for child in &mut children {
        child
            .wait_ready(CHILD_BOUND)
            .unwrap_or_else(|error| panic!("{error}"));
    }
    for child in &mut children {
        child.release();
    }
    for child in children {
        child.finish();
    }

    // A fresh instance after the writers exited: the restart view.
    let auth = read_json(&home.join("auth.json"));
    let auth = auth.as_object().expect("auth object");
    assert_eq!(auth.len(), 2 + CHILDREN * WRITES_PER_CHILD);
    assert_eq!(auth["oauth-provider"], oauth);
    assert_eq!(auth["future-provider"], unknown);
    let storage = AuthStorage::load(&home.join("auth.json"));
    for index in 0..CHILDREN {
        for write in 0..WRITES_PER_CHILD {
            assert_eq!(
                storage.read(&format!("child{index}-{write}")),
                Some(Credential::ApiKey {
                    key: Some(format!("secret-{index}-{write}")),
                    env: None
                })
            );
        }
    }
    let settings = read_json(&home.join("settings.json"));
    assert_eq!(settings["theme"], "dark");
    assert_eq!(settings["packages"], json!(["npm:x"]));
    let levels = settings["modelThinkingLevels"].as_object().expect("levels");
    assert_eq!(levels.len(), 1 + CHILDREN * WRITES_PER_CHILD);
    let loaded = Settings::load(&home);
    assert_eq!(
        loaded.model_thinking_level("keep", "me"),
        Some(ModelThinkingLevel::High)
    );
    assert_eq!(
        loaded.model_thinking_level("child3", "model14"),
        Some(ModelThinkingLevel::Low)
    );
    let mut names = entries(&home);
    names.retain(|name| name.ends_with(".tmp"));
    assert!(names.is_empty(), "temporary files left: {names:?}");
}

#[test]
fn a_lock_held_by_a_killed_process_does_not_block_later_updates() {
    let dir = TempDir::new("json-store-killed");
    let home = dir.path();
    let path = home.join("auth.json");
    fs::write(&path, "{}").expect("write");
    let mut holder = ChildProcess::spawn("holder", home, 0);
    holder
        .wait_ready(CHILD_BOUND)
        .unwrap_or_else(|error| panic!("{error}"));

    let waiting =
        JsonFileStore::new(path.clone(), LIMIT).with_lock_timeout(Duration::from_millis(20));
    assert!(matches!(set_a(&waiting), Err(StoreError::Locked { .. })));
    assert_eq!(fs::read_to_string(&path).expect("read"), "{}");

    assert!(!holder.kill().success());
    // The lock file remains, but the OS released the dead holder's lock.
    assert!(home.join(".auth.json.lock").exists());
    let mut storage = AuthStorage::load(&path);
    storage
        .set_api_key("after", "key")
        .expect("update after kill");
    assert_eq!(
        read_json(&path),
        json!({ "after": { "type": "api_key", "key": "key" } })
    );
}
