//! A real console control event while the CLI's output pipe remains full.
//! The console belongs to a helper process, so parallel tests never change
//! the test runner's console or control handlers.

use std::fs::File;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle};
use std::os::windows::process::CommandExt;

use bake_process::windows_job::Job;
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetConsoleCP,
};
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::Pipes::{CreatePipe, GetNamedPipeInfo, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP};

use super::{
    Command, Duration, Instant, OwnedChild, RUN_TIMEOUT, Read, Reply, Server, Stdio, Workspace,
    Write,
};

const HELPER: &str = "BAKE_TEST_PRINT_CONSOLE_HELPER";
const CASE: &str = "windows::a_full_json_output_pipe_can_be_cancelled_without_a_reader";
const STOP_BOUND: Duration = Duration::from_secs(10);

/// The existing process owner accounts for every descendant and waits for
/// its process handle; a timeout or assertion cannot orphan the CLI.
struct HelperTree(Job);

impl Drop for HelperTree {
    fn drop(&mut self) {
        let result = self.0.terminate_and_settle(STOP_BOUND);
        if std::thread::panicking() {
            if let Err(error) = result {
                eprintln!("console helper cleanup could not settle: {error:?}");
            }
        } else {
            assert!(result.is_ok(), "console helper cleanup: {result:?}");
        }
    }
}

#[test]
fn a_full_json_output_pipe_can_be_cancelled_without_a_reader() {
    if std::env::var_os(HELPER).is_some() {
        // No descendant starts until the parent has attached this helper to
        // its job. EOF before admission makes the helper fail without work.
        let mut admitted = [0];
        io::stdin()
            .read_exact(&mut admitted)
            .expect("job admission");
        assert_eq!(admitted, [1]);
        // SAFETY: GetConsoleCP has no pointer arguments or ownership effects.
        assert_ne!(unsafe { GetConsoleCP() }, 0, "helper has its own console");
        cancel_full_pipe();
        return;
    }

    let tree = HelperTree(Job::new().expect("owned console helper job"));
    let mut helper = OwnedChild(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", CASE, "--nocapture"])
            .env(HELPER, "1")
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("console helper"),
    );
    // SAFETY: both handles are live. The helper is waiting on admission and
    // has not created the CLI or any other descendant.
    assert_ne!(
        unsafe {
            AssignProcessToJobObject(tree.0.as_handle().as_raw_handle(), helper.0.as_raw_handle())
        },
        0,
        "assign console helper: {}",
        io::Error::last_os_error()
    );
    helper
        .0
        .stdin
        .take()
        .expect("admission pipe")
        .write_all(&[1])
        .expect("admit helper");
    let deadline = Instant::now() + RUN_TIMEOUT * 2;
    let status = loop {
        tree.0.pump().expect("account helper processes");
        if let Some(status) = helper.0.try_wait().expect("helper status") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "console helper exceeded its bound"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "console helper failed: {status}");
}

fn small_pipe() -> (File, File, u32) {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    // SAFETY: the output slots are valid. Null attributes make both new
    // handles non-inheritable; Command duplicates only the selected writer.
    assert_ne!(
        unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 4096) },
        0,
        "create output pipe: {}",
        io::Error::last_os_error()
    );
    // SAFETY: successful CreatePipe returned two fresh, independently owned handles.
    let (read, write) = unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) };
    let mut incoming = 0;
    let mut outgoing = 0;
    // SAFETY: the read handle and output allocations are live.
    assert_ne!(
        unsafe {
            GetNamedPipeInfo(
                read.as_raw_handle(),
                std::ptr::null_mut(),
                &mut outgoing,
                &mut incoming,
                std::ptr::null_mut(),
            )
        },
        0,
        "query output pipe: {}",
        io::Error::last_os_error()
    );
    // CreatePipe's size is a suggestion; observe the actual capacity.
    let capacity = incoming.max(outgoing);
    assert!(
        (1..=64 * 1024).contains(&capacity),
        "small pipe capacity: {capacity}"
    );
    (read, write, capacity)
}

fn available(pipe: &File) -> u32 {
    let mut bytes = 0;
    // SAFETY: this test is the sole user of the read handle. No competing
    // read can block this synchronous peek or consume its reported bytes.
    assert_ne!(
        unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut bytes,
                std::ptr::null_mut(),
            )
        },
        0,
        "peek output pipe: {}",
        io::Error::last_os_error()
    );
    bytes
}

fn cancel_full_pipe() {
    let (mut reader, writer, capacity) = small_pipe();
    let mut reply = Reply::sse();
    let delta = "x".repeat((capacity as usize * 16).max(64 * 1024));
    reply.body = reply.body.replace("Hello from", &delta);
    let server = Server::start(reply);
    let workspace = Workspace::new("blocked-json-windows", server.port);
    let errors = workspace.root.join("stderr.log");
    let mut child = OwnedChild(
        workspace
            .command(
                &[
                    "--mode",
                    "json",
                    "--no-tools",
                    "--system-prompt",
                    "Reply briefly.",
                    "Say hi",
                ],
                false,
            )
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .env("TOKIO_WORKER_THREADS", "1")
            .stdout(writer)
            .stderr(File::create(&errors).expect("CLI error log"))
            .spawn()
            .expect("bake-rs starts"),
    );
    let deadline = Instant::now() + RUN_TIMEOUT;
    // Drain startup only. The large delta proves that signal registration
    // and the provider request have completed before we stop reading.
    loop {
        let count = available(&reader);
        if count > 0 {
            let mut bytes = [0; 4096];
            let count = (count as usize).min(bytes.len());
            let read = reader.read(&mut bytes[..count]).expect("read startup");
            assert_ne!(read, 0, "stdout closed before the provider delta");
            if bytes[..read].windows(32).any(|bytes| bytes == [b'x'; 32]) {
                break;
            }
        }
        assert!(
            child.0.try_wait().expect("status").is_none(),
            "CLI exited during startup"
        );
        assert!(
            Instant::now() < deadline,
            "no provider delta reached stdout"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!server.requests().is_empty());
    while available(&reader) < capacity {
        assert!(
            child.0.try_wait().expect("status").is_none(),
            "CLI exited before filling stdout"
        );
        assert!(Instant::now() < deadline, "stdout did not fill");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Only this child group shares the event; CTRL_C_EVENT cannot be targeted
    // this way. The test runner lives in a different console.
    // SAFETY: the unreaped child's id is the group we created above, and it
    // shares this helper's console. The pipe reader remains open and unread.
    assert_ne!(
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.0.id()) },
        0,
        "send Ctrl+Break: {}",
        io::Error::last_os_error()
    );
    let deadline = Instant::now() + STOP_BOUND;
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("status") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "Ctrl+Break did not stop unread stdout"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        status.code(),
        Some(130),
        "{}",
        std::fs::read_to_string(errors).unwrap_or_default()
    );
    assert!(
        available(&reader) >= capacity,
        "cancellation did not need a reader"
    );
    let files = workspace.session_files();
    assert_eq!(files.len(), 1);
    let session = bake_coding_agent::session::SessionManager::open(&files[0], None, None)
        .expect("the stopped session can be reopened");
    drop(session);
}
