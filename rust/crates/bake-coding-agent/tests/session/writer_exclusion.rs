//! Writer exclusion and durable writes, which Pi does not have (scope 03 of
//! the Rust 0.4 roadmap): one writer per session file, takeover after the
//! holder releases, exits, or is killed, torn tails, and rewrites that never
//! leave a half-written file.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bake_coding_agent::session::{
    NewSessionOptions, SessionError, SessionManager, find_most_recent_session,
    load_entries_from_file,
};
use serde_json::json;

use crate::support::{TempDir, assistant_msg, roles, text_of, user_msg};

/// A persisted session holding one exchange, and its manager.
fn written_session(dir: &TempDir) -> (SessionManager, PathBuf) {
    let mut session =
        SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
            .expect("create");
    session.append_message(user_msg("one")).expect("append");
    session
        .append_message(assistant_msg("answer"))
        .expect("append");
    let file = session.session_file().expect("file").to_path_buf();
    (session, file)
}

fn lock_of(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
        .collect();
    names.sort();
    names
}

fn assert_locked(result: Result<SessionManager, SessionError>, file: &Path) {
    match result {
        Err(SessionError::Locked(path)) => {
            assert_eq!(path, file);
            assert_eq!(
                SessionError::Locked(path).to_string(),
                format!(
                    "Session file is in use by another writer: {}",
                    file.display()
                )
            );
        }
        other => panic!("expected the file to be locked, got {other:?}"),
    }
}

#[test]
fn a_second_writer_is_refused_and_takes_over_after_a_drop() {
    let dir = TempDir::new("exclusion-drop");
    let (first, file) = written_session(&dir);
    assert_locked(SessionManager::open(&file, None, None), &file);
    assert_locked(
        SessionManager::continue_recent(dir.str(), Some(dir.path())),
        &file,
    );

    // Readers take no lock, and never list the lock file.
    let listed = SessionManager::list(dir.str(), Some(dir.path()), None, None).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, file);
    let all = SessionManager::list_all(Some(dir.path()), None, None).expect("list all");
    assert_eq!(all.len(), 1);
    assert_eq!(
        find_most_recent_session(dir.path(), None),
        Some(file.clone())
    );
    assert_eq!(load_entries_from_file(&file).expect("load").len(), 3);
    assert!(lock_of(&file).exists());

    drop(first);
    let mut second = SessionManager::open(&file, None, None).expect("taken over");
    second.append_message(user_msg("two")).expect("append");
    drop(second);
    let reopened = SessionManager::open(&file, None, None).expect("reopen");
    assert_eq!(
        roles(&reopened.build_session_context().messages),
        ["user", "assistant", "user"]
    );
}

/// A closed writer can be taken over and never writes again: as with any
/// failed write, its entries stay in memory and the leaf moves to them.
#[test]
fn a_closed_writer_writes_nothing_more() {
    let dir = TempDir::new("exclusion-close");
    let (mut first, file) = written_session(&dir);
    first.close_writer();
    let second = SessionManager::open(&file, None, None).expect("taken over");
    let closed = |result: Result<String, SessionError>| {
        assert!(
            matches!(result, Err(SessionError::WriterClosed(ref path)) if *path == file),
            "{result:?}"
        );
    };
    closed(first.append_message(user_msg("late")));
    assert_eq!(
        first
            .leaf_entry()
            .and_then(|entry| entry.message())
            .map(|message| text_of(&message))
            .as_deref(),
        Some("late")
    );
    drop(second);
    // Even with the file free, nothing is written and no lock is taken.
    closed(first.append_thinking_level_change("high"));
    assert_eq!(load_entries_from_file(&file).expect("load").len(), 3);
    drop(SessionManager::open(&file, None, None).expect("still free"));
}

/// Only a session being written gets a lock file: opening a missing path or
/// a file that is not a session leaves none.
#[test]
fn lock_files_are_made_only_for_sessions() {
    let dir = TempDir::new("exclusion-stray");
    let missing = dir.join("missing.jsonl");
    let session = SessionManager::open(&missing, None, None).expect("open missing");
    drop(session);
    let other = dir.join("other.jsonl");
    fs::write(&other, "not a session\n").expect("write");
    assert!(matches!(
        SessionManager::open(&other, None, None),
        Err(SessionError::NotASession(_))
    ));
    assert_eq!(names(dir.path()), ["other.jsonl"]);
}

#[test]
fn a_new_session_and_a_branch_release_the_previous_file() {
    let dir = TempDir::new("exclusion-switch");
    let (mut session, file) = written_session(&dir);
    let leaf = session.leaf_id().expect("leaf").to_owned();
    let branched = session
        .create_branched_session(&leaf)
        .expect("branch")
        .expect("file");
    drop(SessionManager::open(&file, None, None).expect("first file released"));
    assert_locked(SessionManager::open(&branched, None, None), &branched);
    session
        .new_session(NewSessionOptions::default())
        .expect("new");
    drop(SessionManager::open(&branched, None, None).expect("branch released"));
}

#[test]
fn a_fork_holds_its_new_file() {
    let dir = TempDir::new("exclusion-fork");
    let (source, file) = written_session(&dir);
    // The source is read without its lock.
    let fork = SessionManager::fork_from(
        &file,
        dir.str(),
        Some(dir.path()),
        NewSessionOptions::default(),
    )
    .expect("fork");
    let forked = fork.session_file().expect("file").to_path_buf();
    assert_locked(SessionManager::open(&forked, None, None), &forked);
    drop(source);
}

/// What a holder child does once it holds the file.
#[derive(Clone, Copy)]
enum Holder {
    /// Wait to be killed.
    Killed,
    /// Exit normally once stdin has a line.
    Exits,
}

const HOLDER_ENV: &str = "BAKE_TEST_SESSION_HOLDER";
const HOLDING: &str = "bake-test-holding-session";

/// Not a test of its own: the child process of the holder tests, which
/// re-run this test binary with [`HOLDER_ENV`] naming the file to hold.
#[test]
fn session_holder_child() {
    let Some(file) = std::env::var_os(HOLDER_ENV) else {
        return;
    };
    let _held = SessionManager::open(Path::new(&file), None, None).expect("child open");
    println!("{HOLDING}");
    std::io::stdout().flush().expect("flush");
    // Exits when stdin has a line or closes; a killed child never gets here.
    // The parent kills the child at the latest when it drops.
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}

/// A child holding a session file, killed on drop.
struct HolderChild {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl HolderChild {
    fn spawn(file: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "writer_exclusion::session_holder_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(HOLDER_ENV, file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn holder");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("holder stdout");
        // Built before the wait, so a failed wait still kills the child.
        let holder = Self { child, stdin };
        // The reader drains the child's output until it exits, so the child
        // never writes to a closed pipe; libtest prints the marker after the
        // test's name on one line.
        let (held, holding) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    return;
                };
                if line.contains(HOLDING) {
                    let _ = held.send(());
                }
            }
        });
        holding
            .recv_timeout(Duration::from_secs(30))
            .expect("the holder child did not take the session");
        holder
    }

    fn wait(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.child.try_wait().expect("holder status") {
                Some(_) => return,
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => panic!("the holder child did not exit"),
            }
        }
    }

    fn finish(&mut self, how: Holder) {
        match how {
            Holder::Killed => self.child.kill().expect("kill holder"),
            Holder::Exits => {
                let mut stdin = self.stdin.take().expect("holder stdin");
                stdin.write_all(b"exit\n").expect("tell holder");
            }
        }
        self.wait();
    }
}

impl Drop for HolderChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn takes_over_from_a_process(how: Holder, name: &str) {
    let dir = TempDir::new(name);
    let (session, file) = written_session(&dir);
    drop(session);
    let mut holder = HolderChild::spawn(&file);
    assert_locked(SessionManager::open(&file, None, None), &file);
    assert_locked(
        SessionManager::continue_recent(dir.str(), Some(dir.path())),
        &file,
    );
    holder.finish(how);
    // Windows unlocks a dead process's locks soon after it exits, not
    // necessarily before `wait` returns.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut session = loop {
        match SessionManager::open(&file, None, None) {
            Err(SessionError::Locked(_)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => break result.expect("taken over"),
        }
    };
    session.append_message(user_msg("after")).expect("append");
    assert_eq!(session.build_session_context().messages.len(), 3);
}

#[test]
fn a_killed_holder_process_is_taken_over() {
    takes_over_from_a_process(Holder::Killed, "exclusion-killed");
}

#[test]
fn an_exited_holder_process_is_taken_over() {
    takes_over_from_a_process(Holder::Exits, "exclusion-exited");
}

/// A writer killed mid-append leaves a torn last line: the next writer reads
/// the file as Pi's loader does, skipping the fragment, and its appends
/// start on a line of their own.
#[test]
fn a_torn_tail_resumes_and_appends() {
    let dir = TempDir::new("exclusion-torn");
    let (session, file) = written_session(&dir);
    drop(session);
    let mut bytes = fs::read(&file).expect("read");
    let complete = bytes.len();
    bytes.extend_from_slice(br#"{"type":"message","id":"torn","parentId":"#);
    fs::write(&file, &bytes).expect("tear");

    let mut resumed = SessionManager::open(&file, None, None).expect("resume");
    assert_eq!(resumed.build_session_context().messages.len(), 2);
    resumed.append_message(user_msg("after")).expect("append");
    let written = fs::read_to_string(&file).expect("read");
    let tail: Vec<&str> = written[complete..].lines().collect();
    assert_eq!(tail.len(), 2, "{written}");
    assert_eq!(tail[0], r#"{"type":"message","id":"torn","parentId":"#);
    assert!(tail[1].starts_with(r#"{"type":"message""#), "{}", tail[1]);

    // A fragment that appears while the file is held (a failed partial
    // write) is set apart by the next append, and a reader leaves the
    // held file as it is.
    let mut file_handle = fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .expect("open");
    file_handle.write_all(b"{\"partial\":").expect("tear again");
    drop(file_handle);
    let before = fs::read(&file).expect("read");
    load_entries_from_file(&file).expect("load");
    assert_eq!(fs::read(&file).expect("read"), before);
    resumed.append_message(user_msg("last")).expect("append");
    drop(resumed);

    let reopened = SessionManager::open(&file, None, None).expect("reopen");
    let messages = reopened.build_session_context().messages;
    assert_eq!(
        messages.iter().map(text_of).collect::<Vec<_>>(),
        ["one", "answer", "after", "last"]
    );
}

/// Migration rewrites through a temporary file renamed over the session:
/// nothing but the session and its lock is left, and leftovers a crash
/// would leave are never listed.
#[test]
fn migration_rewrites_atomically_and_leftovers_are_not_sessions() {
    let dir = TempDir::new("exclusion-migrate");
    let file = dir.join("v1.jsonl");
    let lines = [
        json!({"type": "session", "id": "v1-session", "timestamp": "2025-01-01T00:00:00Z", "cwd": dir.str()}),
        json!({"type": "message", "timestamp": "2025-01-01T00:00:01Z", "message": {"role": "user", "content": "hi", "timestamp": 1}}),
    ];
    let content: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(&file, &content).expect("write");
    let session = SessionManager::open(&file, None, None).expect("open");
    assert_eq!(session.build_session_context().messages.len(), 1);
    assert!(
        fs::read_to_string(&file)
            .expect("read")
            .contains(r#""version":3"#)
    );
    assert_eq!(names(dir.path()), ["v1.jsonl", "v1.jsonl.lock"]);
    drop(session);

    // A rewrite's temporary file a crash left before its rename, holding a
    // valid session, and a stale lock: neither is a session.
    fs::write(dir.join(".v1.jsonl.0000.tmp"), &content).expect("leftover");
    let listed = SessionManager::list(dir.str(), Some(dir.path()), None, None).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, file);
    assert_eq!(
        find_most_recent_session(dir.path(), None),
        Some(file.clone())
    );
    assert_eq!(
        SessionManager::find_by_id(dir.str(), "v1-session", Some(dir.path())).expect("find"),
        Some(file)
    );
}

#[cfg(unix)]
fn v1_content(cwd: &str, id: &str) -> String {
    let lines = [
        json!({"type": "session", "id": id, "timestamp": "2025-01-01T00:00:00Z", "cwd": cwd}),
        json!({"type": "message", "timestamp": "2025-01-01T00:00:01Z", "message": {"role": "user", "content": "hi", "timestamp": 1}}),
    ];
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// Migration keeps the file's mode, and through a symbolic link migrates
/// the file the link names, leaving the link a link.
#[cfg(unix)]
#[test]
fn migration_keeps_mode_and_links() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("exclusion-mode");
    let file = dir.join("private.jsonl");
    fs::write(&file, v1_content(dir.str(), "private")).expect("write");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    drop(SessionManager::open(&file, None, None).expect("open"));
    let metadata = fs::metadata(&file).expect("stat");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(
        fs::read_to_string(&file)
            .expect("read")
            .contains(r#""version":3"#)
    );

    let real = dir.join("real.jsonl");
    fs::write(&real, v1_content(dir.str(), "real")).expect("write");
    let link = dir.join("link.jsonl");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    drop(SessionManager::open(&link, None, None).expect("open link"));
    assert!(
        fs::symlink_metadata(&link)
            .expect("lstat")
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(&real)
            .expect("read")
            .contains(r#""version":3"#)
    );
}

/// In a read-only directory, a writable session without a lock file is
/// still opened, held, and appended to: the session file itself is locked.
#[cfg(unix)]
#[test]
fn a_read_only_directory_still_writes() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore<'a>(&'a Path);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let dir = TempDir::new("exclusion-read-only");
    let (session, file) = written_session(&dir);
    drop(session);
    fs::remove_file(lock_of(&file)).expect("remove lock");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).expect("chmod");
    let _restore = Restore(dir.path());
    if fs::write(dir.join("probe"), "").is_ok() {
        // Running as root: directory permissions do not apply.
        return;
    }
    let mut held = SessionManager::open(&file, None, None).expect("open");
    assert_locked(SessionManager::open(&file, None, None), &file);
    held.append_message(user_msg("two")).expect("append");
    drop(held);
    let reopened = SessionManager::open(&file, None, None).expect("reopen");
    assert_eq!(reopened.build_session_context().messages.len(), 3);
    assert!(!lock_of(&file).exists());
}
