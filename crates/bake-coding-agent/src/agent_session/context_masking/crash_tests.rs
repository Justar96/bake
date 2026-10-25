//! A killed writer leaves a reconstructable prefix of complete replacements.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::*;

const CHILD_PATH: &str = "BAKE_TEST_MASKING_PATH";
const CHILD_PHASE: &str = "BAKE_TEST_MASKING_PHASE";
const READY: &str = "bake-masking-writer-held";
const CHILD_TEST: &str = "agent_session::context_masking::tests::crash::masking_writer_child";

fn hold() {
    println!("{READY}");
    std::io::stdout().flush().unwrap();
    // A vanished parent closes stdin; no helper survives its owner.
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    std::process::exit(91);
}

#[test]
fn masking_writer_child() {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let phase: usize = std::env::var(CHILD_PHASE).unwrap().parse().unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let h = harness();
            let manager = SessionManager::open(Path::new(&path), None, None).unwrap();
            let mut options = options(&h, manager, ".");
            options.settings.context_masking = true;
            let session = AgentSession::create(options).await.unwrap();
            if phase == 3 {
                session.core.hooks().after_context_masking = Some(Box::new(hold));
            } else {
                lock(&session.session).arm_group_fault(Fault {
                    line: phase,
                    kind: FaultKind::Hold(0, Box::new(hold)),
                });
            }
            session.core.mask_context(None).await.unwrap();
            panic!("the held child must be killed by its parent");
        });
}

struct Writer {
    child: Child,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Writer {
    fn start(path: &Path, phase: usize) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads", "1"])
            .env(CHILD_PATH, path)
            .env(CHILD_PHASE, phase.to_string())
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
                .name("masking-child-output".into())
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
            .expect("the child reached its masking write boundary");
        writer
    }

    fn kill(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "the child is held before the parent's kill"
        );
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
        }
        #[cfg(windows)]
        assert_eq!(status.code(), Some(1));
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
fn killed_before_during_or_after_masking_reopens_complete_tool_pairs() {
    for phase in [0, 1, 3] {
        let dir = TempDir::new("masking-crash");
        let mut manager = new_file_session(&dir);
        prepared(&mut manager);
        let path = manager.session_file().unwrap().to_owned();
        drop(manager);
        let mut child = Writer::start(&path, phase);
        child.kill();
        drop(child);
        let reopened = SessionManager::open(&path, None, None).unwrap();
        assert_eq!(
            reopened
                .entries()
                .iter()
                .filter(|entry| entry.entry_type() == "context_edit")
                .count(),
            phase
        );
        let projection = reopened.build_session_projection();
        assert_eq!(batches(&projection).len(), RETAINED_BATCHES + 3);
        assert_eq!(
            projection
                .messages
                .iter()
                .filter(|message| message.as_json().get("content")
                    == Some(&json!([{ "type":"text", "text":REPLACEMENT }])))
                .count(),
            phase
        );
        assert!(
            reopened.entries().iter().any(|entry| entry
                .get("message")
                .is_some_and(|message| message.to_string().contains("old observation 0"))),
            "original results remain intact"
        );
    }
}
