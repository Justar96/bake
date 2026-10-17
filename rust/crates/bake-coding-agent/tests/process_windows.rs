//! Bake: an owned process tree on Windows (`child_process::windows`). The
//! command is born in a kill-on-close job; a normal exit clears that limit
//! before the job's handle closes, so an intentionally started background
//! process keeps running, as Pi leaves it; before that detach, the death of
//! the process that owns the job ends the tree; an abort terminates the job
//! and returns once it is empty.
//!
//! Each case starts its background process with PowerShell's
//! `Start-Process`, which writes its pid to a file in the case's own
//! directory. The test opens the pid at once and keeps the handle, so later
//! checks name that process and no other; it terminates it at the end
//! whatever happened.
#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bake_ai::AbortController;
use bake_coding_agent::exec::{ExecOptions, exec_command};
use bake_coding_agent::owned_work::WorkGate;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    TerminateProcess, WaitForSingleObject,
};

const BOUND: Duration = Duration::from_secs(30);
const HELPER: &str = "BAKE_PROCESS_WINDOWS_HELPER";

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).expect("random");
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("bake-process-windows-{id}"));
        std::fs::create_dir(&dir).expect("temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A background process this test holds by handle and ends when dropped.
struct Background(OwnedHandle);

impl Background {
    /// Waits for `file` to hold a pid, then opens that process.
    async fn open(file: &Path) -> Self {
        let deadline = Instant::now() + BOUND;
        let pid: u32 = loop {
            if let Some(pid) = std::fs::read_to_string(file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "no background pid was written");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        // SAFETY: plain call; a null handle means the open failed.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        assert!(
            !handle.is_null(),
            "the background process could not be opened"
        );
        // SAFETY: a fresh handle owned here.
        Self(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn running(&self) -> bool {
        // SAFETY: a live handle opened with SYNCHRONIZE.
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_TIMEOUT }
    }

    async fn ended_within(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        loop {
            // SAFETY: as above.
            if unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        // SAFETY: a live handle with terminate access.
        unsafe {
            TerminateProcess(self.0.as_raw_handle(), 1);
        }
    }
}

/// PowerShell that starts a hidden `ping` and writes its pid to `file`,
/// then runs `then`.
fn powershell(file: &Path, then: &str) -> Vec<OsString> {
    vec![
        "-NoProfile".into(),
        "-Command".into(),
        format!(
            "$p = Start-Process -PassThru -WindowStyle Hidden ping -ArgumentList '-n','600','127.0.0.1'; \
             Set-Content -NoNewline -Path '{}' -Value $p.Id; {then}",
            file.display()
        )
        .into(),
    ]
}

// A normal exit detaches its job: the background process outlives the call
// and the owner's close, and the call's result is the normal one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_normal_exit_leaves_its_background_running() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let file = dir.0.join("pid");
    let owner = WorkGate::default();
    let result = exec_command(
        "powershell".as_ref(),
        &powershell(&file, "exit 0"),
        &dir.0,
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    )
    .await;
    let background = Background::open(&file).await;
    let closed = tokio::time::timeout(BOUND, owner.close()).await.is_ok();
    assert_eq!(result.code, 0, "{result:?}");
    assert!(!result.killed && !result.aborted);
    assert!(closed);
    assert!(
        background.running(),
        "the normal exit's background was killed"
    );
}

// An abort terminates the job and returns once it has no active process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_ends_and_awaits_the_whole_job() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let file = dir.0.join("pid");
    let abort = AbortController::new();
    let args = powershell(&file, "Start-Sleep -Seconds 600");
    let cwd = dir.0.clone();
    let signal = abort.signal();
    let call = tokio::spawn(async move {
        exec_command(
            "powershell".as_ref(),
            &args,
            &cwd,
            ExecOptions {
                signal: Some(signal),
                ..ExecOptions::default()
            },
        )
        .await
    });
    let background = Background::open(&file).await;
    abort.abort();
    let result = tokio::time::timeout(BOUND, call)
        .await
        .expect("the call returns")
        .expect("joined");
    // The call returned only once the job was empty: already ended.
    let ended = background.ended_within(Duration::ZERO).await;
    assert!(result.aborted && result.killed);
    assert!(ended, "the call returned while a job member ran");
}

// Before a normal exit detaches it, the job ends with the process that
// owns it: a helper (this binary's `helper_case`) runs a command that
// starts a background process and keeps running; the test terminates the
// helper and the background process must end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owners_death_before_detach_ends_the_tree() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let file = dir.0.join("pid");
    let mut helper = tokio::process::Command::new(std::env::current_exe().expect("exe"))
        .args(["--exact", "helper_case", "--ignored", "--nocapture"])
        .env(HELPER, &file)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("helper");
    let background = Background::open(&file).await;
    helper.start_kill().expect("terminate the helper");
    let _ = tokio::time::timeout(BOUND, helper.wait()).await;
    assert!(
        background.ended_within(BOUND).await,
        "the tree outlived the process that owned its job"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by the_owners_death_before_detach_ends_the_tree"]
async fn helper_case() {
    let file = PathBuf::from(std::env::var_os(HELPER).expect("run by its test"));
    let cwd = file.parent().expect("dir").to_path_buf();
    // Never returns: the test terminates this process.
    let _ = exec_command(
        "powershell".as_ref(),
        &powershell(&file, "Start-Sleep -Seconds 600"),
        &cwd,
        ExecOptions::default(),
    )
    .await;
}
