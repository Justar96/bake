//! Parent death stops a compiling helper. The test re-runs this binary as
//! a stand-in parent that starts a host on a slow component and reports
//! the helper's pid once it compiles; the test then kills that parent and
//! waits for the orphaned helper to exit.
//!
//! On Linux the test process is a child subreaper (which is why this test
//! has its own binary), so the orphan becomes its child and its exit code
//! is observable: 72 is the helper's watchdog seeing its input close while
//! the compiler runs, where a finished compile would instead fail writing
//! its answer with 74. On Windows the Job's kill-on-close may end the
//! helper first, so any exit counts there; on macOS only the exit is
//! observed.

#[path = "wasm_plugin_host/fixtures.rs"]
mod fixtures;

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::time::Duration;

use bake_coding_agent::plugins::wasm::{HostLimits, HostStatus};
use fixtures::*;
use tokio::sync::watch;

const ROLE: &str = "BAKE_WASM_TEST_PARENT_ROLE";

/// A component that takes a few seconds to compile: well beyond the time
/// the watchdog needs, within the 512 MiB budget.
fn slow() -> bake_coding_agent::plugins::wasm::WasmPackage {
    package("slow", &["write"], slow_component(1600, 1000))
}

fn role_limits() -> HostLimits {
    HostLimits {
        memory_budget: 512 << 20,
        max_component_bytes: 16 << 20,
        compile_timeout: Duration::from_secs(300),
        ..limits()
    }
}

/// The stand-in parent; a no-op unless the test started it.
#[test]
fn parent_role() {
    if std::env::var_os(ROLE).is_none() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let (status, mut seen) = watch::channel(HostStatus::Starting);
        let package = slow();
        let limits = role_limits();
        let command = command();
        let start = try_start_with(&command, &package, &limits, Some(status));
        let announce = async {
            wait_status(&mut seen, |state| matches!(state, HostStatus::Compiling(_))).await;
            let current = *seen.borrow();
            let HostStatus::Compiling(Some(pid)) = current else {
                panic!("the compiling helper has a pid");
            };
            println!("helper-pid={pid}");
            std::future::pending::<()>().await;
        };
        tokio::select! {
            result = start => panic!("the start finished: {result:?}"),
            () = announce => {}
        }
    });
}

#[test]
fn parent_death_stops_a_compiling_helper() {
    if std::env::var_os(ROLE).is_some() {
        return;
    }
    #[cfg(target_os = "linux")]
    // SAFETY: plain call; it changes only who adopts this process's orphans.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );

    let mut parent = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "parent_role",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(ROLE, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the stand-in parent");
    let stdout = parent.stdout.take().expect("stdout");
    let mut lines = std::io::BufReader::new(stdout).lines();
    let pid: u32 = loop {
        let line = lines
            .next()
            .expect("the parent reports the helper before exiting")
            .expect("read");
        // libtest may print `test parent_role ... ` on the same line.
        if let Some((_, pid)) = line.split_once("helper-pid=") {
            break pid.trim().parse().expect("a pid");
        }
    };
    #[cfg(windows)]
    let process = windows::open(pid);

    parent.kill().expect("kill the parent");
    parent.wait().expect("reap the parent");

    #[cfg(target_os = "linux")]
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut status = 0;
            // SAFETY: plain call; the orphan is this process's child now.
            let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
            let _ = sender.send((waited, status));
        });
        let Ok((waited, status)) = receiver.recv_timeout(Duration::from_secs(60)) else {
            // SAFETY: the unreaped orphan's pid; the waiting thread reaps it.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            panic!("the helper outlived its parent by 60 s");
        };
        assert_eq!(waited, pid as libc::pid_t);
        assert!(libc::WIFEXITED(status), "status {status:#x}");
        assert_eq!(
            libc::WEXITSTATUS(status),
            72,
            "the watchdog ended the compile"
        );
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !process_gone(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "the helper outlived its parent by 60 s"
            );
            std::thread::yield_now();
        }
    }
    #[cfg(windows)]
    windows::wait(process, Duration::from_secs(60));
}

#[cfg(windows)]
mod windows {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    pub fn open(pid: u32) -> HANDLE {
        // SAFETY: plain call; the handle is closed by `wait`.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        assert!(!handle.is_null(), "open the helper");
        handle
    }

    pub fn wait(handle: HANDLE, timeout: Duration) {
        // SAFETY: a live handle opened by `open`, closed here.
        let waited = unsafe { WaitForSingleObject(handle, timeout.as_millis() as u32) };
        // SAFETY: as above.
        unsafe { CloseHandle(handle) };
        assert_eq!(waited, WAIT_OBJECT_0, "the helper outlived its parent");
    }
}
