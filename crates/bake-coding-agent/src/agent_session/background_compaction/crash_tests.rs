//! A killed real writer leaves either the old context or a replayable commit.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

use super::*;
use crate::test_support::TempDir;

const CHILD_PATH: &str = "BAKE_TEST_BACKGROUND_COMMIT_PATH";
const CHILD_PHASE: &str = "BAKE_TEST_BACKGROUND_COMMIT_PHASE";
const READY: &str = "bake-background-commit-held";
const CHILD_TEST: &str =
    "agent_session::background_compaction::tests::lifecycle::crash::background_commit_child";

#[test]
fn background_commit_child() {
    let Some(file) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let after = std::env::var(CHILD_PHASE).unwrap() == "after";
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let manager = SessionManager::open(Path::new(&file), None, None).unwrap();
            let (session, faux, _) = fixture_with_session(None, true, Some(manager)).await;
            summaries(&faux);
            session
                .core
                .background_compaction_boundary(true)
                .await
                .unwrap();
            finished(&session).await;
            let held: Box<dyn FnOnce() + Send> = Box::new(|| {
                println!("{READY}");
                std::io::stdout().flush().unwrap();
                // Only EOF, when the parent goes away, ends the hold; a read
                // error is reported and retried, never taken for EOF.
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::stdin().lock().read_line(&mut line) {
                        Ok(0) => std::process::exit(91),
                        Ok(_) => {}
                        Err(error) => {
                            eprintln!("held writer: stdin: {error}");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
            });
            if after {
                session.core.hooks().after_compaction_commit = Some(held);
            } else {
                session.core.hooks().before_compaction_commit = Some(held);
            }
            session
                .core
                .background_compaction_boundary(false)
                .await
                .unwrap();
            panic!("parent failed to stop its held writer");
        });
}

struct Writer {
    child: Child,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Writer {
    fn start(path: &Path, after: bool) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads", "1"])
            .env(CHILD_PATH, path)
            .env(CHILD_PHASE, if after { "after" } else { "before" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut writer = Self {
            child,
            reader: None,
        };
        let output = writer.child.stdout.take().unwrap();
        let (ready_tx, ready) = mpsc::channel();
        writer.reader = Some(
            std::thread::Builder::new()
                .name("background-commit-output".into())
                .spawn(move || {
                    for line in BufReader::new(output).lines() {
                        let Ok(line) = line else {
                            return;
                        };
                        if line.contains(READY) {
                            let _ = ready_tx.send(());
                        }
                    }
                })
                .unwrap(),
        );
        ready
            .recv_timeout(Duration::from_secs(60))
            .expect("child reached the requested commit boundary");
        writer
    }

    fn kill(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "child must still be held before the parent kill"
        );
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                status.signal(),
                Some(9),
                "parent SIGKILL ended the child: {status:?}"
            );
        }
        #[cfg(windows)]
        assert_eq!(
            status.code(),
            Some(1),
            "parent TerminateProcess ended the child"
        );
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn killing_a_background_writer_before_or_after_commit_reopens_the_expected_context() {
    for after in [false, true] {
        let dir = TempDir::new("background-crash");
        let mut manager = SessionManager::create(
            &dir.path().to_string_lossy(),
            Some(&dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .unwrap();
        for text in ["first request", "second request", "third request"] {
            append_turn(&mut manager, text);
        }
        let file = manager.session_file().unwrap().to_owned();
        drop(manager);
        let mut writer = Writer::start(&file, after);
        writer.kill();
        drop(writer);
        let reopened = SessionManager::open(&file, None, None).expect("crashed session reopens");
        assert_eq!(
            reopened
                .entries()
                .iter()
                .filter(|entry| entry.entry_type() == "compaction")
                .count(),
            usize::from(after)
        );
        let projection = reopened.build_session_projection();
        let serialized = serde_json::to_string(&projection.messages).unwrap();
        assert_eq!(serialized.contains("Preserve the task"), after);
        assert!(
            serialized.contains("current response"),
            "the retained tail survives both sides of commit"
        );
        if !after {
            assert!(
                serialized.contains("first request"),
                "the old prefix remains before commit"
            );
        }
    }
}
