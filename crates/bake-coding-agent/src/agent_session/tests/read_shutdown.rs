//! A real blocking native read survives the hard abort of its Agent task.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::task::Poll;
use std::time::Duration;

use super::*;
use crate::owned_work::WorkGate;
use crate::tools::index::{ToolsOptions, create_tool};

struct HeldWriter {
    path: PathBuf,
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HeldWriter {
    fn start(path: &Path) -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is NUL terminated and the private path does not exist.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        let fifo = path.to_path_buf();
        let thread = std::thread::spawn(move || {
            let writer = OpenOptions::new().write(true).open(fifo).unwrap();
            let _ = entered.send(());
            let _ = held.recv();
            drop(writer);
        });
        (
            Self {
                path: path.to_path_buf(),
                release: Some(release),
                thread: Some(thread),
            },
            ready,
        )
    }
}

impl Drop for HeldWriter {
    fn drop(&mut self) {
        // Also unblocks an open if setup failed before the reader reached it.
        let _anchor = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.path);
        // Any later pass must open a regular file, including failed setup.
        let _ = std::fs::remove_file(&self.path);
        let _ = File::create(&self.path);
        self.release.take();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

#[tokio::test]
async fn hard_agent_shutdown_keeps_native_read_owned_until_the_worker_returns() {
    let dir = TempDir::new("owned-native-read");
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = dir.join("held");
    let (writer, entered) = HeldWriter::start(&path);
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call(
                "read",
                json!({"path":"held"}),
                Some("read_1"),
            )],
            StopReason::ToolUse,
        )
        .into(),
    ]);
    let work = WorkGate::default();
    let tool = create_tool(
        "read",
        dir.path(),
        &ToolsOptions {
            owner: Some(work.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            work: Some(work.clone()),
            tools: vec![Arc::new(tool)],
            active_tool_names: vec!["read".into()],
            ..options(&h, new_file_session(&dir), &cwd)
        })
        .await
        .unwrap(),
    );
    let owner = Arc::clone(&session);
    let prompt = tokio::spawn(async move { owner.prompt("read held", Vec::new()).await });
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .unwrap()
        .unwrap();
    // The worker holds the original FIFO; subsequent opens use a regular file.
    // Cleanup therefore cannot get stuck in read's second pass after EOF.
    std::fs::remove_file(&path).unwrap();
    File::create(&path).unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.agent.shutdown())
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .unwrap()
        .unwrap();
    let mut drained = Box::pin(work.close());
    let waiting =
        std::future::poll_fn(|cx| Poll::Ready(drained.as_mut().poll(cx).is_pending())).await;
    drop(writer);
    if waiting {
        tokio::time::timeout(Duration::from_secs(10), drained)
            .await
            .unwrap();
    }
    session.shutdown().await;
    let file = session.session_file().unwrap();
    let reopened = SessionManager::open(&file, None, None).unwrap();
    drop(reopened);
    assert!(
        waiting,
        "shutdown declared quiescence while native read still held its file"
    );
}
