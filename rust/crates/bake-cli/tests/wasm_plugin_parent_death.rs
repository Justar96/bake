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
//!
//! Every step is bounded: each announcement and the helper's exit get
//! 60 s, and cleanup, which runs on panic too, kills and waits for the
//! parent and an identified helper for 10 s each (see `Run` for its
//! limits).

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
            // The pid as early as the host reports it, so the test can own
            // the helper before it compiles; `Spawned` may be skipped.
            wait_status(&mut seen, |state| {
                matches!(state, HostStatus::Spawned(_) | HostStatus::Compiling(_))
            })
            .await;
            let current = *seen.borrow();
            let (HostStatus::Spawned(Some(pid)) | HostStatus::Compiling(Some(pid))) = current
            else {
                panic!("the started helper has a pid: {current:?}");
            };
            println!("helper-pid={pid}");
            wait_status(&mut seen, |state| matches!(state, HostStatus::Compiling(_))).await;
            println!("helper-compiling");
            std::future::pending::<()>().await;
        };
        tokio::select! {
            result = start => panic!("the start finished: {result:?}"),
            () = announce => {}
        }
    });
}

/// How long each step may take before the test fails and cleans up.
const STEP: Duration = Duration::from_secs(60);
/// How long cleanup waits for each process it killed.
const CLEANUP: Duration = Duration::from_secs(10);

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

    let mut run = Run::spawn();
    let pid = run.expect_line("helper-pid=");
    let pid: u32 = pid.trim().parse().expect("a pid");
    // Identified while the stand-in parent lives and holds the helper
    // unreaped, so the pid still names the helper it announced; from here
    // cleanup can kill it.
    run.helper = Some(platform::Helper::identify(pid, run.parent_pid()));
    run.expect_line("helper-compiling");

    run.kill_parent();

    let helper = run.helper.as_mut().expect("identified above");
    let exit = helper.wait(STEP);
    assert!(exit.is_some(), "the helper outlived its parent by 60 s");
    #[cfg(target_os = "linux")]
    {
        let status = exit.expect("checked above");
        assert!(libc::WIFEXITED(status), "status {status:#x}");
        assert_eq!(
            libc::WEXITSTATUS(status),
            72,
            "the watchdog ended the compile"
        );
    }
}

/// The stand-in parent, its stdout reader, and the helper once
/// identified. Dropping it, on success or panic, kills whichever of the
/// two processes is still running and waits for each within [`CLEANUP`];
/// it signals only the parent it spawned and a helper whose identity was
/// verified. The reader is joined if it has finished within [`CLEANUP`]
/// (the parent's death closes its stdout); otherwise cleanup reports it
/// and leaves the thread detached.
///
/// Limits: a failure before the helper is identified (no announcement, or
/// a failed identity check) leaves that helper to its parent-death
/// watchdog. On Linux it is then reaped here if it exits within
/// [`CLEANUP`], and otherwise reported; on macOS and Windows it is not
/// observed (on Windows the Job's kill-on-close still ends it).
struct Run {
    parent: std::process::Child,
    parent_reaped: bool,
    lines: std::sync::mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    helper: Option<platform::Helper>,
}

impl Run {
    fn spawn() -> Self {
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
        let (sender, lines) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            parent,
            parent_reaped: false,
            lines,
            reader: Some(reader),
            helper: None,
        }
    }

    fn parent_pid(&self) -> u32 {
        self.parent.id()
    }

    /// The rest of the parent's next line containing `marker`, within
    /// [`STEP`].
    fn expect_line(&self, marker: &str) -> String {
        let deadline = std::time::Instant::now() + STEP;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("the parent printed no {marker} within 60 s")
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the parent closed its output before {marker}")
                }
            };
            // libtest may print `test parent_role ... ` on the same line.
            if let Some((_, rest)) = line.split_once(marker) {
                return rest.to_owned();
            }
        }
    }

    /// Kills the stand-in parent and reaps it within [`CLEANUP`].
    fn kill_parent(&mut self) {
        if self.parent_reaped {
            return;
        }
        // An exited but unreaped child keeps its pid, so this signals
        // only the process spawned above.
        let _ = self.parent.kill();
        let deadline = std::time::Instant::now() + CLEANUP;
        loop {
            match self.parent.try_wait() {
                Ok(Some(_)) => {
                    self.parent_reaped = true;
                    return;
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                outcome => panic!("the killed parent was not reaped: {outcome:?}"),
            }
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // The helper first, while its identity is still verified.
        if let Some(helper) = self.helper.as_mut() {
            helper.kill();
            if helper.wait(CLEANUP).is_none() {
                eprintln!("cleanup: helper {} survived SIGKILL", helper.pid());
            }
        }
        if !self.parent_reaped {
            let _ = self.parent.kill();
            let deadline = std::time::Instant::now() + CLEANUP;
            while !matches!(self.parent.try_wait(), Ok(Some(_)) | Err(_)) {
                if std::time::Instant::now() >= deadline {
                    eprintln!("cleanup: parent {} was not reaped", self.parent.id());
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            self.parent_reaped = true;
        }
        // A helper orphaned to this subreaper, identified or not, is reaped
        // here once it exits (through its watchdog, or the kill above).
        #[cfg(target_os = "linux")]
        platform::reap_orphans(CLEANUP);
        if let Some(reader) = self.reader.take() {
            let deadline = std::time::Instant::now() + CLEANUP;
            while !reader.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                let _ = reader.join();
            } else {
                eprintln!("cleanup: the parent's stdout stayed open");
            }
        }
    }
}

/// The helper as this test knows it: verified to be the parent's
/// `bake-rs` helper child while that parent lived, and signalled only
/// through that identity.
#[cfg(target_os = "linux")]
mod platform {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::time::Duration;

    use bake_coding_agent::plugins::wasm::HELPER_ARG;

    pub struct Helper {
        pid: u32,
        fd: OwnedFd,
        status: Option<i32>,
    }

    impl Helper {
        /// Opens a pidfd for `pid` and checks, through `/proc`, that it is
        /// a child of `parent` running the helper, and that the pidfd's
        /// process was still alive after that check.
        pub fn identify(pid: u32, parent: u32) -> Self {
            // SAFETY: plain syscall; a non-negative result is a new
            // descriptor owned here.
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
            assert!(
                raw >= 0,
                "pidfd_open({pid}): {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: as above.
            let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let is_helper = cmdline
                .split(|byte| *byte == 0)
                .any(|arg| arg == HELPER_ARG.as_bytes());
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            // The fields after the parenthesised command: state, then ppid.
            let ppid = stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(1))
                .and_then(|field| field.parse::<u32>().ok());
            let helper = Self {
                pid,
                fd,
                status: None,
            };
            assert!(
                is_helper && ppid == Some(parent) && !helper.exited(Duration::ZERO),
                "pid {pid} is not the parent's live helper: ppid {ppid:?}"
            );
            helper
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        fn exited(&self, timeout: Duration) -> bool {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                let mut poll = libc::pollfd {
                    fd: self.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd.
                let ready =
                    unsafe { libc::poll(&mut poll, 1, left.as_millis().min(60_000) as i32) };
                match ready {
                    1.. => return true,
                    0 if left.is_zero() => return false,
                    0 => {}
                    _ if std::io::Error::last_os_error().kind()
                        == std::io::ErrorKind::Interrupted => {}
                    _ => return false,
                }
            }
        }

        /// Waits up to `timeout` for the orphaned helper's exit and reaps
        /// it (it is this subreaper's child once its parent is gone); the
        /// wait status, or `None` if it is still running.
        pub fn wait(&mut self, timeout: Duration) -> Option<i32> {
            if self.status.is_none() && self.exited(timeout) {
                let mut status = 0;
                // SAFETY: the pid of an exited process the pidfd names;
                // unreaped, so it cannot have been reused.
                let waited =
                    unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG) };
                // While the parent still lives it reaps the helper instead.
                self.status = Some(if waited == self.pid as libc::pid_t {
                    status
                } else {
                    -1
                });
            }
            self.status
        }

        pub fn kill(&mut self) {
            if self.status.is_some() {
                return;
            }
            // SAFETY: plain syscall on the owned pidfd; an exited process
            // makes it fail harmlessly.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }

    /// Reaps exited children adopted by this subreaper until none remain
    /// or `timeout` passes.
    pub fn reap_orphans(timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let mut status = 0;
            // SAFETY: plain call; this binary's only children are the
            // parent (already reaped) and orphans from it.
            let waited = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if waited < 0 {
                return;
            }
            if waited == 0 {
                if std::time::Instant::now() >= deadline {
                    eprintln!("cleanup: an orphan of the parent is still running");
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// On macOS an orphan goes to launchd, so only its exit is observed. Its
/// identity is its pid and start time, read while the parent holds it
/// unreaped and checked again before any signal. That check and the
/// `kill` are not atomic: a pid reused in between would be signalled,
/// which needs the helper to exit and launchd to reap it and a new
/// process to take its pid within that window.
#[cfg(target_os = "macos")]
mod platform {
    use std::time::Duration;

    use super::process_gone;

    pub struct Helper {
        pid: u32,
        start: (u64, u64),
        gone: bool,
    }

    fn info(pid: u32) -> Option<libc::proc_bsdinfo> {
        // SAFETY: a zeroed `proc_bsdinfo` is valid output space of the
        // size passed.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: as above.
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                // Keep an exited but unreaped helper visible too.
                1,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
        (read == size).then_some(info)
    }

    impl Helper {
        pub fn identify(pid: u32, parent: u32) -> Self {
            let info = info(pid).unwrap_or_else(|| panic!("no process {pid}"));
            let comm: Vec<u8> = info
                .pbi_comm
                .iter()
                .take_while(|c| **c != 0)
                .map(|c| *c as u8)
                .collect();
            assert!(
                info.pbi_ppid == parent && comm == b"bake-rs",
                "pid {pid} is not the parent's helper: ppid {}, {:?}",
                info.pbi_ppid,
                String::from_utf8_lossy(&comm)
            );
            Self {
                pid,
                start: (info.pbi_start_tvsec, info.pbi_start_tvusec),
                gone: false,
            }
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn wait(&mut self, timeout: Duration) -> Option<i32> {
            let deadline = std::time::Instant::now() + timeout;
            while !self.gone {
                if process_gone(self.pid)
                    || info(self.pid).map(|i| (i.pbi_start_tvsec, i.pbi_start_tvusec))
                        != Some(self.start)
                {
                    self.gone = true;
                } else if std::time::Instant::now() >= deadline {
                    return None;
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Some(0)
        }

        pub fn kill(&mut self) {
            if self.gone {
                return;
            }
            let same =
                info(self.pid).map(|i| (i.pbi_start_tvsec, i.pbi_start_tvusec)) == Some(self.start);
            if same {
                // SAFETY: plain call on the pid just checked to be the
                // helper by its start time.
                unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
            } else {
                self.gone = true;
            }
        }
    }
}

/// On Windows the Job's kill-on-close may end the helper first, so any
/// exit counts. The handle is opened while the parent holds the helper's
/// own handle, so the pid names the helper, and its image is checked.
#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    };

    pub struct Helper {
        pid: u32,
        handle: HANDLE,
        gone: bool,
    }

    impl Helper {
        pub fn identify(pid: u32, _parent: u32) -> Self {
            // SAFETY: plain call; the handle is closed on drop.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    0,
                    pid,
                )
            };
            assert!(!handle.is_null(), "open the helper");
            let helper = Self {
                pid,
                handle,
                gone: false,
            };
            let mut name = [0u16; 1024];
            let mut len = name.len() as u32;
            // SAFETY: a live handle and a buffer of `len` units.
            let ok = unsafe {
                QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, name.as_mut_ptr(), &mut len)
            };
            let image = String::from_utf16_lossy(&name[..len as usize]);
            assert!(
                ok != 0 && image.to_ascii_lowercase().ends_with("bake-rs.exe"),
                "pid {pid} is not the helper: {image}"
            );
            helper
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn wait(&mut self, timeout: Duration) -> Option<i32> {
            if !self.gone {
                // SAFETY: the live handle owned here.
                let waited = unsafe {
                    WaitForSingleObject(
                        self.handle,
                        timeout.as_millis().min(u32::MAX as u128 - 1) as u32,
                    )
                };
                self.gone = waited == WAIT_OBJECT_0;
            }
            self.gone.then_some(0)
        }

        pub fn kill(&mut self) {
            if !self.gone {
                // SAFETY: the owned handle; a best-effort signal whose
                // completion `wait` observes.
                unsafe { TerminateProcess(self.handle, 1) };
            }
        }
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            // SAFETY: the handle opened in `identify`, closed once.
            unsafe { CloseHandle(self.handle) };
        }
    }
}
