//! Real enforcement through the Windows restricted-token runner, derived
//! from `packages/sandbox/sandbox-windows-acl/tests/runner.spec.ts` and
//! `acl.spec.ts`, `packages/subprocess/subprocess-local/tests/
//! windows-job.spec.ts`, and the scope-04 proof list.
//!
//! This binary is also the confined program: a first argument of
//! [`PROBE_ARG`] runs [`probe_main`], which performs one exact filesystem
//! or process operation and reports it, so every check observes a real
//! effect (or its absence) on disk or in the process table, without a
//! shell's own wording. Each scenario owns its directories under Cargo's
//! per-target temporary directory, whose workspace and outside roots grant
//! this user full access ([`user_scratch`]); the runner's own private temp
//! directories live under the OS temporary directory or, for direct runner
//! runs, under a directory the scenario owns. Negative controls run the same
//! probe unconfined and see it succeed.
//!
//! The default provider refuses on Windows, because this runner leaves
//! deletion unconfined; [`the_default_provider_refuses`] asserts that
//! refusal and [`deletion_escapes_the_write_restricted_token`] keeps the
//! counterexample behind it. Every other scenario runs the runner through an
//! explicit chain, to qualify what it does confine. There is no skip.
//!
//! Every process a scenario starts is owned by a guard that kills and awaits
//! it on drop, a panic included. Handshakes are bounded; [`Lines`] reads
//! only bytes available on its exclusively owned stdout pipe and starts no
//! reader thread.
//! A descendant's end is asserted on a handle the scenario opened while the
//! descendant's pid was pinned by its parent's handle, never on a pid
//! reopened after the parent exited: there an open that fails, or a query
//! that is refused, is not evidence of an exit.
//!
//! [`mechanisms`] records, without qualifying anything, how this token and
//! two candidate replacements (low integrity, AppContainer) treat the same
//! deletion, rename, read, and network effects.

use std::ffi::OsString;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use bake_sandbox::backends::windows_acl::{
    self, HELPER_ARG as RUNNER_ARG, PRIVATE_TEMP_PREFIX, allow_aces_naming, workspace_write_sid,
};
use bake_sandbox::backends::{LocalProvider, Runner};
use bake_sandbox::{Enforcement, SandboxMode, SandboxPolicy, SandboxProvider, default_provider};

use super::{Run, Scratch, harness, run_argv, runtime};

#[path = "windows_mechanisms.rs"]
mod mechanisms;

#[path = "windows_appcontainer.rs"]
mod appcontainer;

/// The hidden first argument that turns this binary into the probe.
pub const PROBE_ARG: &str = "__bake-test-probe";

/// Exit status of a probe whose operation was denied.
const DENIED: i32 = 13;

/// Runs one probe operation and exits: 0 on success, [`DENIED`] with a
/// `denied:` stderr line on `PermissionDenied`, 1 otherwise.
pub fn probe_main(args: Vec<OsString>) -> ! {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.into_string().expect("UTF-8 probe argument"))
        .collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // A mechanism-probe candidate, which inherits no handle, first sends
    // its own output to files the launcher reads back.
    let skip = match refs.as_slice() {
        [mechanisms::OUTPUT_ARG, dir, ..] => {
            if mechanisms::redirect(dir).is_err() {
                std::process::exit(mechanisms::REDIRECT_FAILED);
            }
            2
        }
        _ => 0,
    };
    let result: std::io::Result<i32> = match &refs[skip..] {
        ["write", path, text] => std::fs::write(path, text).map(|()| 0),
        ["read", path] => std::fs::read_to_string(path).map(|text| {
            print!("{text}");
            0
        }),
        ["mkdir", path] => std::fs::create_dir(path).map(|()| 0),
        ["rename", from, to] => std::fs::rename(from, to).map(|()| 0),
        ["delete", path] => std::fs::remove_file(path).map(|()| 0),
        // One raw deletion primitive, so a result names the exact Win32 path.
        ["win32-delete", method, path] => win32::delete(method, path).map(|()| 0),
        // `CreateFileW(path, mask)` alone: which access the token is granted.
        ["open", mask, path] => win32::open(mask, path).map(|()| 0),
        ["token"] => win32::token_report().map(|report| {
            println!("{report}");
            0
        }),
        ["hardlink", existing, new] => std::fs::hard_link(existing, new).map(|()| 0),
        ["env", name] => {
            println!("{}", std::env::var(name).unwrap_or_default());
            Ok(0)
        }
        ["write-temp"] => {
            let path =
                PathBuf::from(std::env::var_os("TEMP").unwrap_or_default()).join("probe.txt");
            std::fs::write(&path, "t").map(|()| {
                println!("{}", path.display());
                0
            })
        }
        ["exit", code] => Ok(code.parse().expect("exit code")),
        ["echo", out, err] => {
            println!("{out}");
            eprintln!("{err}");
            Ok(0)
        }
        // A child probe with inherited standard handles, awaited.
        ["spawn", rest @ ..] => std::process::Command::new(std::env::current_exe().expect("exe"))
            .arg(PROBE_ARG)
            .args(rest)
            .status()
            .map(|status| status.code().unwrap_or(1)),
        // After a line (or EOF) on stdin, a parked grandchild left
        // running; this process then exits. The wait lets a negative control
        // put this process in its Job before the grandchild exists.
        ["orphan"] => {
            let _ = std::io::stdin().lock().read_line(&mut String::new());
            spawn_parked().map(|pid| {
                println!("grandchild {pid}");
                0
            })
        }
        // A write into the private TEMP, reported, then held until stdin
        // closes, so a second run can act while this one still owns it.
        ["write-temp-and-wait"] => {
            let path = PathBuf::from(std::env::var_os("TEMP").unwrap_or_default()).join("held.txt");
            std::fs::write(&path, "held").map(|()| {
                println!("{}", path.display());
                let _ = std::io::stdout().flush();
                let _ = std::io::stdin().read_to_end(&mut Vec::new());
                0
            })
        }
        // After a line (or EOF) on stdin, a parked grandchild whose handle
        // this process keeps, so its pid cannot be reused, until a second
        // line (or EOF): a test can open it by pid, pinned, before this
        // process exits and leaves it orphaned.
        ["orphan-pinned"] => {
            let mut stdin = std::io::stdin().lock();
            let _ = stdin.read_line(&mut String::new());
            spawn_parked_child().map(|grandchild| {
                println!(
                    "grandchild {} child {}",
                    grandchild.id(),
                    std::process::id()
                );
                let _ = std::io::stdout().flush();
                let _ = stdin.read_line(&mut String::new());
                // Released only now: the pid stayed pinned until here.
                drop(grandchild);
                0
            })
        }
        // A parked grandchild, then this process parks too.
        ["orphan-and-park"] => spawn_parked().map(|pid| {
            println!("grandchild {pid} child {}", std::process::id());
            let _ = std::io::stdout().flush();
            loop {
                std::thread::park();
            }
        }),
        ["park"] => loop {
            std::thread::park();
        },
        ["mechanism", rest @ ..] => mechanisms::probe(rest),
        other => panic!("unknown probe operation {other:?}"),
    };
    let code = match result {
        Ok(code) => code,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("denied: {error}");
            DENIED
        }
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

fn spawn_parked() -> std::io::Result<u32> {
    spawn_parked_child().map(|child| child.id())
}

/// A parked probe holding none of this process's standard handles.
fn spawn_parked_child() -> std::io::Result<std::process::Child> {
    std::process::Command::new(std::env::current_exe()?)
        .args([PROBE_ARG, "park"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

// ---------------------------------------------------------------------------
// Harness glue.

/// Every Windows scenario. None is skipped.
pub fn tests() -> Vec<harness::Test> {
    let scenarios: &[(&str, fn())] = &[
        ("the_default_provider_refuses", the_default_provider_refuses),
        (
            "deletion_escapes_the_write_restricted_token",
            deletion_escapes_the_write_restricted_token,
        ),
        (
            "an_explicit_chain_selects_the_partial_runner",
            an_explicit_chain_selects_the_partial_runner,
        ),
        (
            "read_only_denies_writes_and_keeps_reads",
            read_only_denies_writes_and_keeps_reads,
        ),
        (
            "workspace_write_allows_effects_inside",
            workspace_write_allows_effects_inside,
        ),
        (
            "workspace_write_denies_effects_outside",
            workspace_write_denies_effects_outside,
        ),
        (
            "each_run_gets_a_private_temp_directory",
            each_run_gets_a_private_temp_directory,
        ),
        ("descendants_stay_confined", descendants_stay_confined),
        (
            "descendants_end_with_the_command",
            descendants_end_with_the_command,
        ),
        (
            "an_owned_runner_killed_mid_run_settles_its_tree",
            an_owned_runner_killed_mid_run_settles_its_tree,
        ),
        (
            "a_killed_runner_takes_its_tree",
            a_killed_runner_takes_its_tree,
        ),
        ("junction_escapes_are_denied", junction_escapes_are_denied),
        (
            "hard_links_are_the_documented_partial_boundary",
            hard_links_are_the_documented_partial_boundary,
        ),
        (
            "a_failed_spawn_is_a_runner_failure_and_cleans_up",
            a_failed_spawn_is_a_runner_failure_and_cleans_up,
        ),
        (
            "runner_cli_failures_grant_nothing",
            runner_cli_failures_grant_nothing,
        ),
        (
            "repeated_runs_reuse_one_standing_grant",
            repeated_runs_reuse_one_standing_grant,
        ),
        (
            "read_only_leaves_a_standing_grant_inert",
            read_only_leaves_a_standing_grant_inert,
        ),
        (
            "exit_codes_and_output_pass_through",
            exit_codes_and_output_pass_through,
        ),
    ];
    scenarios
        .iter()
        .map(|(name, scenario)| {
            let scenario = *scenario;
            harness::Test::new(format!("windows-acl::{name}"), scenario)
        })
        .chain(mechanisms::tests())
        .chain(appcontainer::tests())
        .collect()
}

fn exe() -> String {
    std::env::current_exe()
        .expect("exe")
        .into_os_string()
        .into_string()
        .expect("UTF-8 exe path")
}

fn text(path: &Path) -> String {
    path.to_str().expect("UTF-8 path").to_owned()
}

/// `[exe, PROBE_ARG, ops...]`.
fn probe(ops: &[&str]) -> Vec<String> {
    let mut argv = vec![exe(), PROBE_ARG.to_owned()];
    argv.extend(ops.iter().map(|op| (*op).to_owned()));
    argv
}

/// The runner through an explicit chain: the default provider refuses.
fn provider() -> LocalProvider {
    LocalProvider::with_chain(vec![Runner::WindowsAcl])
}

/// Runs the probe `ops` confined under `policy` in `cwd`.
fn confined(policy: &SandboxPolicy, cwd: &Path, ops: &[&str]) -> Run {
    let argv = probe(ops);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    run_argv(&provider(), &refs, policy, cwd)
}

/// Runs the probe `ops` unconfined: the negative control.
fn unconfined(ops: &[&str]) -> std::process::Output {
    std::process::Command::new(exe())
        .arg(PROBE_ARG)
        .args(ops)
        .stdin(Stdio::null())
        .output()
        .expect("unconfined probe")
}

fn assert_ok(run: &Run, what: &str) {
    assert_eq!(
        run.code,
        Some(0),
        "{what}: must succeed\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
}

fn assert_denied(run: &Run, what: &str) {
    assert_eq!(
        run.code,
        Some(DENIED),
        "{what}: must be denied\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
    assert!(
        run.denied(),
        "{what}: stderr must carry the denial dialect: {}",
        run.stderr
    );
    assert_eq!(
        run.runner_failure(),
        None,
        "{what}: a denial is not a runner failure"
    );
}

fn workspace_and_outside() -> (Scratch, Scratch) {
    (user_scratch("acl-ws"), user_scratch("acl-out"))
}

/// A scratch root whose DACL grants this user full access, inherited by
/// everything under it: the runner requires a user-owned workspace, and the
/// deletion measurements in `windows_acl` assume such roots. The runner's
/// `LUA_TOKEN` makes Administrators deny-only, so where directories grant
/// access only through that group, as on GitHub's Windows runners, which
/// run as the built-in Administrator, the normal access check would deny
/// every effect and the denial scenarios would pass without reaching the
/// restricting SIDs.
fn user_scratch(label: &str) -> Scratch {
    let scratch = Scratch::outside(label);
    let user = win32::user_sid().expect("this process's user SID");
    let output = std::process::Command::new("icacls")
        .arg(&scratch.0)
        .args(["/grant", &format!("*{user}:(OI)(CI)F"), "/q"])
        .stdin(Stdio::null())
        .output()
        .expect("icacls");
    assert!(
        output.status.success(),
        "icacls /grant on {}: {}\n{}",
        scratch.0.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    scratch
}

fn ww(workspace: &Scratch) -> SandboxPolicy {
    SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace.0)
}

fn ro(workspace: &Scratch) -> SandboxPolicy {
    SandboxPolicy::new(SandboxMode::ReadOnly, &workspace.0)
}

fn ws_sid(workspace: &Scratch) -> String {
    workspace_write_sid(&text(&workspace.0))
}

/// The bound on every handshake, exit, and cleanup wait.
const BOUND: Duration = Duration::from_secs(30);

fn millis(budget: Duration) -> u32 {
    u32::try_from(budget.as_millis()).unwrap_or(u32::MAX)
}

/// Whether `handle` (opened with `SYNCHRONIZE`) is signaled within `budget`.
fn signaled_within(handle: std::os::windows::io::RawHandle, budget: Duration) -> bool {
    use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    // SAFETY: the caller passes a live handle with SYNCHRONIZE.
    match unsafe { WaitForSingleObject(handle, millis(budget)) } {
        WAIT_OBJECT_0 => true,
        WAIT_FAILED => panic!("WaitForSingleObject: {}", std::io::Error::last_os_error()),
        _ => false,
    }
}

/// The sole reader of one child's anonymous stdout pipe. Peeking before a
/// bounded read keeps handshake waits cancellable without a reader thread.
/// No other thread uses this handle: a synchronous PeekNamedPipe may block
/// behind another operation on the same handle.
struct Lines {
    pipe: std::process::ChildStdout,
    pending: Vec<u8>,
}

/// The longest handshake this fixture accepts.
const LINE_CAP: u64 = 4096;

impl Lines {
    fn new(pipe: std::process::ChildStdout) -> Self {
        Self {
            pipe,
            pending: Vec::new(),
        }
    }

    fn next(&mut self) -> Result<String, String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        let started = std::time::Instant::now();
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                if end as u64 >= LINE_CAP {
                    return Err(format!("handshake exceeds {LINE_CAP} bytes"));
                }
                return Ok(String::from_utf8_lossy(
                    &self.pending.drain(..=end).collect::<Vec<_>>(),
                )
                .into_owned());
            }
            if self.pending.len() as u64 >= LINE_CAP {
                return Err(format!("handshake exceeds {LINE_CAP} bytes"));
            }
            let mut available = 0;
            // SAFETY: this exclusively owned read handle is live; no
            // concurrent read can consume the bytes this call reports.
            let ok = unsafe {
                PeekNamedPipe(
                    self.pipe.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(format!(
                    "stdout ended or could not be queried: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if available > 0 {
                let mut bytes = [0; 4096];
                let length = (available as usize).min(bytes.len());
                let read = self
                    .pipe
                    .read(&mut bytes[..length])
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("stdout ended before a handshake".into());
                }
                self.pending.extend_from_slice(&bytes[..read]);
            } else if started.elapsed() >= BOUND {
                return Err(format!("no line within {BOUND:?}"));
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// A child this test started, and the reader of its standard output once
/// [`Owned::first_line`] or [`Owned::next_line`] started one. Dropped, a
/// panic included, it is killed and awaited within [`BOUND`], and then its
/// stdout handle is closed.
struct Owned(std::process::Child, Option<Lines>);

impl Owned {
    fn spawn(command: &mut std::process::Command) -> Self {
        Self(command.spawn().expect("spawn"), None)
    }

    /// The exit status, if the child exits within `budget`.
    fn wait_within(&mut self, budget: Duration) -> Option<std::process::ExitStatus> {
        use std::os::windows::io::AsRawHandle;
        signaled_within(self.0.as_raw_handle(), budget).then(|| self.0.wait().expect("reap"))
    }

    /// The next stdout line, within [`BOUND`].
    fn next_line(&mut self) -> Result<String, String> {
        if self.1.is_none() {
            let stdout = self.0.stdout.take().expect("piped stdout");
            self.1 = Some(Lines::new(stdout));
        }
        self.1.as_mut().expect("a reader").next()
    }

    /// The first stdout line, within [`BOUND`].
    fn first_line(&mut self) -> String {
        assert!(self.1.is_none(), "the first line was already read");
        self.next_line()
            .unwrap_or_else(|error| panic!("a handshake: {error}"))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let reaped = self.wait_within(BOUND).is_some();
        if !std::thread::panicking() {
            assert!(
                reaped,
                "child {} did not exit within the bound after kill",
                self.0.id()
            );
        }
        // The stdout handle drops after this; no background read survives.
    }
}

/// Whether the process behind `handle` (opened with
/// `PROCESS_QUERY_LIMITED_INFORMATION`) runs the executable at `image`.
fn is_image(handle: std::os::windows::io::RawHandle, image: &Path) -> bool {
    use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;
    let mut name = vec![0u16; 32_768];
    let mut size = name.len() as u32;
    // SAFETY: `name` holds `size` units.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut size) };
    assert_ne!(
        ok,
        0,
        "QueryFullProcessImageNameW: {}",
        std::io::Error::last_os_error()
    );
    let running = PathBuf::from(String::from_utf16_lossy(&name[..size as usize]));
    bake_sandbox::canonical_path(&running) == bake_sandbox::canonical_path(image)
}

/// A descendant this test learned of from a handshake, held by handle so
/// its pid cannot be reused while held. Dropped, it is terminated and
/// awaited within [`BOUND`].
struct OwnedPid(std::os::windows::io::OwnedHandle, u32);

impl OwnedPid {
    /// Opens `pid`, which must still be this test binary.
    fn open(pid: u32) -> Self {
        Self::open_image(pid, &std::env::current_exe().expect("exe"))
    }

    /// Opens `pid`, which must still run the executable at `image`.
    fn open_image(pid: u32, image: &Path) -> Self {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        };
        // SAFETY: plain call; null means no such process.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        assert!(
            !raw.is_null(),
            "open pid {pid}: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: a fresh handle owned here.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        assert!(
            is_image(handle.as_raw_handle(), image),
            "pid {pid} is not {}",
            image.display()
        );
        Self(handle, pid)
    }

    fn running(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        !signaled_within(self.0.as_raw_handle(), Duration::ZERO)
    }

    fn exited_within(&self, budget: Duration) -> bool {
        use std::os::windows::io::AsRawHandle;
        signaled_within(self.0.as_raw_handle(), budget)
    }
}

impl Drop for OwnedPid {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Threading::TerminateProcess;
        // SAFETY: a live handle opened with PROCESS_TERMINATE.
        unsafe { TerminateProcess(self.0.as_raw_handle(), 1) };
        let exited = self.exited_within(BOUND);
        if !std::thread::panicking() {
            assert!(
                exited,
                "pid {} did not exit within the bound after termination",
                self.1
            );
        }
    }
}

/// A kill-on-close Job this test owns: an unconfined negative control joins
/// it before the control starts any descendant, and each mechanism-probe
/// candidate is born in one. It is the production
/// [`bake_process::windows_job::Job`], whose completion port is associated
/// before anything joins it. Its handle is never inheritable, so this
/// process holds the only one and an abrupt end of the test closes it and
/// kills every member.
///
/// [`TestJob::finish`] terminates every member and waits, within [`BOUND`],
/// for the Job's account to settle: every process the Job reported held by
/// handle until signalled, never the active count alone. This is cleanup,
/// not an assertion oracle: each scenario asserts on the handles it pinned
/// itself. A Job dropped without `finish` (a panic included) does the same
/// as best effort: it panics with the error on a normal path and only
/// prints it while unwinding, so cleanup never double-panics.
struct TestJob {
    job: bake_process::windows_job::Job,
    finished: bool,
}

impl TestJob {
    fn new() -> Self {
        Self {
            job: bake_process::windows_job::Job::new().expect("a kill-on-close Job and its port"),
            finished: false,
        }
    }

    /// The Job's handle, borrowed for a creation's job list and queries.
    fn raw(&self) -> std::os::windows::io::RawHandle {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        self.job.as_handle().as_raw_handle()
    }

    fn assign(&self, child: &Owned) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: two live handles.
        let ok = unsafe { AssignProcessToJobObject(self.raw(), child.0.as_raw_handle()) };
        assert_ne!(
            ok,
            0,
            "AssignProcessToJobObject: {}",
            std::io::Error::last_os_error()
        );
    }

    /// The ids of the Job's live members. A failed query, or a list that
    /// stays incomplete after the buffer grows, is an error, never an empty
    /// list.
    fn members(&self) -> std::io::Result<Vec<u32>> {
        use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList, QueryInformationJobObject,
        };
        let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList)
            / size_of::<usize>();
        let mut capacity = 256usize;
        for _ in 0..4 {
            let mut buffer = vec![0usize; header + capacity];
            // SAFETY: `buffer` holds the bytes passed, aligned for the struct.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.raw(),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * size_of::<usize>()) as u32,
                    std::ptr::null_mut(),
                )
            };
            let complete = if ok == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("QueryInformationJobObject (process ids): {error}"),
                    ));
                }
                false
            } else {
                true
            };
            // SAFETY: the call succeeded or reported ERROR_MORE_DATA, and
            // either way wrote the header.
            let (assigned, listed) = unsafe {
                let list = &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
                (
                    list.NumberOfAssignedProcesses as usize,
                    list.NumberOfProcessIdsInList as usize,
                )
            };
            if complete && listed == assigned && listed <= capacity {
                return Ok(buffer[header..][..listed]
                    .iter()
                    .map(|&id| id as u32)
                    .collect());
            }
            capacity = assigned.max(capacity * 2);
        }
        Err(std::io::Error::other(
            "QueryInformationJobObject (process ids): the list stayed incomplete",
        ))
    }

    /// Terminates every member and waits within [`BOUND`] until the Job's
    /// account settles. It never panics, so a drop while unwinding can call
    /// it.
    fn terminate_and_reap(&self) -> Result<(), String> {
        self.job.terminate_and_settle(BOUND).map_err(|verdict| {
            format!("the Job's account did not settle within {BOUND:?} of termination: {verdict}")
        })
    }

    /// Terminates and reaps every member, reporting any failure. Closing
    /// the handle afterwards kills whatever a failed reap left behind.
    fn finish(mut self) -> Result<(), String> {
        self.finished = true;
        self.terminate_and_reap()
    }
}

impl Drop for TestJob {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Err(error) = self.terminate_and_reap() {
            if std::thread::panicking() {
                eprintln!("test Job cleanup while unwinding: {error}");
            } else {
                panic!("test Job cleanup: {error}");
            }
        }
    }
}

/// Runs the runner directly with `args` before `--` and `command` after it.
fn runner(args: &[&str], command: &[String]) -> std::process::Output {
    std::process::Command::new(exe())
        .arg(RUNNER_ARG)
        .args(args)
        .arg("--")
        .args(command)
        .stdin(Stdio::null())
        .output()
        .expect("runner")
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| entry.expect("entry").path())
        .collect()
}

fn mklink_junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J", &text(link), &text(target)])
        .stdout(Stdio::null())
        .status()
        .expect("mklink");
    assert!(status.success(), "mklink /J failed");
}

// ---------------------------------------------------------------------------
// Scenarios.

fn the_default_provider_refuses() {
    let (workspace, _outside) = workspace_and_outside();
    let argv: Vec<OsString> = probe(&["exit", "0"])
        .into_iter()
        .map(OsString::from)
        .collect();
    for policy in [ro(&workspace), ww(&workspace)] {
        for provider in [
            default_provider(),
            std::sync::Arc::new(LocalProvider::new()),
        ] {
            let error = runtime()
                .block_on(provider.confine(&argv, &policy))
                .expect_err("the default provider must refuse on Windows");
            assert_eq!(
                error.detail.as_deref(),
                Some(windows_acl::DEFAULT_REFUSAL),
                "{error}"
            );
        }
    }
}

fn an_explicit_chain_selects_the_partial_runner() {
    let provider = provider();
    assert_eq!(
        runtime().block_on(provider.selected()),
        Some((Runner::WindowsAcl, Enforcement::Partial))
    );
    let (workspace, _outside) = workspace_and_outside();
    let run = confined(&ro(&workspace), &workspace.0, &["exit", "0"]);
    assert_ok(&run, "a read-only no-op");
    assert_eq!(run.command.enforcement, Enforcement::Partial);
    assert_eq!(run.command.program, OsString::from(exe()));
    assert_eq!(
        run.command.args.first().map(OsString::as_os_str),
        Some(RUNNER_ARG.as_ref())
    );
}

fn read_only_denies_writes_and_keeps_reads() {
    let (workspace, outside) = workspace_and_outside();
    let seeded = workspace.write("seed.txt", "seed");
    let policy = ro(&workspace);
    for (what, ops) in [
        (
            "create in the workspace",
            vec!["write", &text(&workspace.join("new.txt")) as &str, "x"],
        ),
        (
            "overwrite in the workspace",
            vec!["write", &text(&seeded), "x"],
        ),
        (
            "create outside",
            vec!["write", &text(&outside.join("new.txt")), "x"],
        ),
        (
            "mkdir in the workspace",
            vec!["mkdir", &text(&workspace.join("d"))],
        ),
        // Deletion is not denied: see the deletion counterexample.
    ] {
        assert_denied(&confined(&policy, &workspace.0, &ops), what);
    }
    assert!(!workspace.join("new.txt").exists());
    assert!(!outside.join("new.txt").exists());
    assert!(!workspace.join("d").exists());
    assert_eq!(std::fs::read_to_string(&seeded).expect("seed"), "seed");
    let run = confined(&policy, &workspace.0, &["read", &text(&seeded)]);
    assert_ok(&run, "a read-only read");
    assert_eq!(run.stdout, "seed");
    // Negative control: the same probe unconfined writes.
    let control = unconfined(&["write", &text(&workspace.join("new.txt")), "x"]);
    assert!(control.status.success());
    assert!(workspace.join("new.txt").exists());
}

fn workspace_write_allows_effects_inside() {
    let (workspace, _outside) = workspace_and_outside();
    let policy = ww(&workspace);
    let nested = workspace.join("nested");
    std::fs::create_dir(&nested).expect("nested");
    let existing = workspace.write("existing.txt", "old");
    for (what, ops) in [
        (
            "create",
            vec!["write", &text(&workspace.join("a.txt")) as &str, "a"],
        ),
        (
            "create nested",
            vec!["write", &text(&nested.join("b.txt")), "b"],
        ),
        ("overwrite", vec!["write", &text(&existing), "new"]),
        ("mkdir", vec!["mkdir", &text(&workspace.join("made"))]),
        (
            "rename",
            vec![
                "rename",
                &text(&workspace.join("a.txt")),
                &text(&workspace.join("c.txt")),
            ],
        ),
        ("delete", vec!["delete", &text(&nested.join("b.txt"))]),
    ] {
        assert_ok(&confined(&policy, &workspace.0, &ops), what);
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("c.txt")).expect("c"),
        "a"
    );
    assert!(!workspace.join("a.txt").exists());
    assert!(!nested.join("b.txt").exists());
    assert!(workspace.join("made").is_dir());
    assert_eq!(std::fs::read_to_string(&existing).expect("existing"), "new");
}

fn workspace_write_denies_effects_outside() {
    let (workspace, outside) = workspace_and_outside();
    let policy = ww(&workspace);
    let victim = outside.write("victim.txt", "keep");
    let inside = workspace.write("inside.txt", "in");
    let ambient = std::env::temp_dir().join(format!("bake-acl-ambient-{}", std::process::id()));
    for (what, ops) in [
        (
            "create outside",
            vec!["write", &text(&outside.join("new.txt")) as &str, "x"],
        ),
        ("overwrite outside", vec!["write", &text(&victim), "x"]),
        // Deleting or moving an outside file in is not denied: see the
        // deletion counterexample.
        (
            "rename the workspace out",
            vec!["rename", &text(&inside), &text(&outside.join("moved.txt"))],
        ),
        (
            "write the ambient temp root",
            vec!["write", &text(&ambient), "x"],
        ),
    ] {
        assert_denied(&confined(&policy, &workspace.0, &ops), what);
    }
    assert_eq!(std::fs::read_to_string(&victim).expect("victim"), "keep");
    assert!(!outside.join("new.txt").exists());
    assert!(!outside.join("moved.txt").exists());
    assert!(!workspace.join("moved.txt").exists());
    assert!(inside.exists());
    assert!(!ambient.exists());
    // The ambient temp root and the outside directory never gained an ACE.
    let sid = ws_sid(&workspace);
    assert_eq!(allow_aces_naming(&outside.0, &sid), Ok(0));
    assert_eq!(allow_aces_naming(&std::env::temp_dir(), &sid), Ok(0));
    // Negative control.
    let control = unconfined(&["write", &text(&outside.join("new.txt")), "x"]);
    assert!(control.status.success());
}

fn each_run_gets_a_private_temp_directory() {
    let (workspace, _outside) = workspace_and_outside();
    let policy = ww(&workspace);
    let temp_root = bake_sandbox::canonical_path(&std::env::temp_dir());
    let is_private = |path: &Path| {
        path.parent() == Some(temp_root.as_path())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(PRIVATE_TEMP_PREFIX))
    };
    // Run A writes its private TEMP and holds it while run B, on the same
    // workspace, tries to write into it.
    let argv: Vec<OsString> = probe(&["write-temp-and-wait"])
        .into_iter()
        .map(OsString::from)
        .collect();
    let command = runtime()
        .block_on(provider().confine(&argv, &policy))
        .expect("confine");
    let mut a = Owned::spawn(
        std::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&workspace.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    let held = PathBuf::from(a.first_line().trim());
    let a_temp = held.parent().expect("A's TEMP").to_path_buf();
    assert!(is_private(&a_temp), "{}", a_temp.display());
    assert_eq!(std::fs::read_to_string(&held).expect("A's file"), "held");
    let intruder = a_temp.join("from-b.txt");
    assert_denied(
        &confined(&policy, &workspace.0, &["write", &text(&intruder), "x"]),
        "B creates in A's TEMP",
    );
    assert_denied(
        &confined(&policy, &workspace.0, &["write", &text(&held), "x"]),
        "B overwrites A's file",
    );
    assert!(!intruder.exists());
    assert_eq!(std::fs::read_to_string(&held).expect("A's file"), "held");
    // B's own private TEMP, a descendant's write included, is writable and
    // distinct.
    let b = confined(&policy, &workspace.0, &["spawn", "write-temp"]);
    assert_ok(&b, "B's descendant writes B's TEMP");
    let b_written = PathBuf::from(b.stdout.trim());
    let b_temp = b_written.parent().expect("B's TEMP");
    assert!(is_private(b_temp), "{}", b_temp.display());
    assert_ne!(b_temp, a_temp.as_path());
    assert!(
        !b_temp.exists(),
        "{} must be removed after B",
        b_temp.display()
    );
    // Releasing A ends its run, which removes its directory.
    drop(a.0.stdin.take());
    let status = a.wait_within(BOUND).expect("A exits within the bound");
    assert!(status.success(), "{status}");
    assert!(
        !a_temp.exists(),
        "{} must be removed after A",
        a_temp.display()
    );
    // Read-only leaves TEMP untouched.
    let run = confined(&ro(&workspace), &workspace.0, &["env", "TEMP"]);
    assert_ok(&run, "read-only TEMP");
    assert_eq!(run.stdout.trim(), std::env::var("TEMP").unwrap_or_default());
    // Under a temp root this test owns, the written private directory and
    // its grant are gone when the runner exits.
    let root = Scratch::outside("acl-temp-root");
    let output = runner(
        &[
            "--workspace",
            &text(&workspace.0),
            "--temp",
            &text(&root.0),
            "--mode",
            "workspace-write",
        ],
        &probe(&["spawn", "write-temp"]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    assert_eq!(
        written.parent().and_then(Path::parent),
        Some(root.0.as_path())
    );
    assert!(
        entries(&root.0).is_empty(),
        "the private directory must be removed"
    );
    assert_eq!(allow_aces_naming(&root.0, &ws_sid(&workspace)), Ok(0));
}

fn descendants_stay_confined() {
    let (workspace, outside) = workspace_and_outside();
    let policy = ww(&workspace);
    let run = confined(
        &policy,
        &workspace.0,
        &["spawn", "write", &text(&outside.join("child.txt")), "x"],
    );
    assert_eq!(run.code, Some(DENIED), "{}", run.stderr);
    assert!(run.denied(), "{}", run.stderr);
    assert!(!outside.join("child.txt").exists());
    let run = confined(
        &policy,
        &workspace.0,
        &["spawn", "write", &text(&workspace.join("child.txt")), "x"],
    );
    assert_ok(&run, "a descendant writes inside");
    assert!(workspace.join("child.txt").exists());
}

/// How many pinned cases [`descendants_end_with_the_command`] and
/// [`an_owned_runner_killed_mid_run_settles_its_tree`] run:
/// `BAKE_SANDBOX_DESCENDANT_RUNS` (1..=100), or 10.
fn descendant_runs() -> usize {
    const DEFAULT: usize = 10;
    match std::env::var("BAKE_SANDBOX_DESCENDANT_RUNS") {
        Err(_) => DEFAULT,
        Ok(text) => match text.parse() {
            Ok(runs @ 1..=100) => runs,
            _ => {
                eprintln!(
                    "BAKE_SANDBOX_DESCENDANT_RUNS={text:?} is not 1..=100; running {DEFAULT}"
                );
                DEFAULT
            }
        },
    }
}

/// The runner's arguments, `--` included, for `mode` on `workspace`, with
/// its private directories under `root`, followed by the probe `ops`.
fn runner_argv(workspace: &Scratch, root: &Scratch, mode: &str, ops: &[&str]) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        RUNNER_ARG.into(),
        "--workspace".into(),
        text(&workspace.0),
        "--temp".into(),
        text(&root.0),
        "--mode".into(),
        mode.into(),
        "--".into(),
    ];
    argv.extend(probe(ops));
    argv
}

/// Parses an `orphan-pinned` handshake, `grandchild <pid> child <pid>`.
fn pinned_pids(line: &str) -> (u32, u32) {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next(), words.next()) {
        (Some("grandchild"), Some(grandchild), Some("child"), Some(child)) => (
            grandchild.parse().expect("grandchild pid"),
            child.parse().expect("child pid"),
        ),
        _ => panic!("handshake {line:?}"),
    }
}

/// Opens an `orphan-pinned` probe's grandchild and the probe itself from
/// its handshake. The probe holds the grandchild's handle until it reads
/// its second line, so the grandchild's pid cannot have been reused when it
/// is opened here; the probe is checked still running after that open.
fn open_pinned(line: &str) -> (OwnedPid, OwnedPid) {
    let (grandchild, child) = pinned_pids(line);
    let grandchild = OwnedPid::open(grandchild);
    let child = OwnedPid::open(child);
    assert!(
        grandchild.running(),
        "the grandchild ended before its release"
    );
    assert!(
        child.running(),
        "the probe ended, releasing its grandchild's pid, before the test opened it"
    );
    (grandchild, child)
}

/// Starts `orphan-pinned` in `process` (its stdin and stdout piped) and
/// opens its pinned grandchild and itself; see [`open_pinned`]. Returns the
/// probe's stdin, whose next line lets the probe exit. A missing handshake
/// fails with `diagnose()`, which can show what the process reported.
fn start_pinned(
    process: &mut Owned,
    diagnose: impl Fn() -> String,
) -> (std::process::ChildStdin, OwnedPid, OwnedPid) {
    let mut stdin = process.0.stdin.take().expect("piped stdin");
    stdin.write_all(b"go\n").expect("go");
    stdin.flush().expect("go");
    let line = match process.next_line() {
        Ok(line) => line,
        Err(error) => {
            drop(stdin);
            let status = process.wait_within(BOUND);
            panic!("no handshake ({error}); exit {status:?}\n{}", diagnose())
        }
    };
    let (grandchild, child) = open_pinned(&line);
    (stdin, grandchild, child)
}

/// The command's grandchild, orphaned when its parent exits, must have
/// ended when the runner returns: checked at once, with no wait, on a
/// handle the test opened while the grandchild's pid was pinned (by its
/// parent's handle), never on a pid reopened after the exit. Repeated
/// [`descendant_runs`] times, alternating the two modes. The runner's
/// earlier drain returned once the Job counted no active process, which
/// happens before a terminated member's handle is signalled; this check
/// then caught the member unsignalled only when the runner's cleanup and
/// exit outran the member's rundown, so against that drain each case could
/// fail but none was certain to (a stochastic detector, not a reliable
/// one). The runner also has to exit 0 with no cleanup line and with its
/// private directory removed, which it does only once the account settles.
fn descendants_end_with_the_command() {
    let (workspace, _outside) = workspace_and_outside();
    let runs = descendant_runs();
    for case in 0..runs {
        let mode = if case % 2 == 0 {
            "read-only"
        } else {
            "workspace-write"
        };
        let root = Scratch::outside("acl-orphan-root");
        let log = Scratch::outside("acl-orphan-log");
        let stderr_path = log.join("stderr.txt");
        let stderr = std::fs::File::create(&stderr_path).expect("stderr file");
        let mut runner = Owned::spawn(
            std::process::Command::new(exe())
                .args(runner_argv(&workspace, &root, mode, &["orphan-pinned"]))
                .current_dir(&workspace.0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(stderr),
        );
        let (mut stdin, grandchild, child) = start_pinned(&mut runner, || {
            format!(
                "case {case} ({mode}): {}",
                std::fs::read_to_string(&stderr_path).unwrap_or_default()
            )
        });
        stdin.write_all(b"opened\n").expect("release");
        drop(stdin);
        let status = runner.wait_within(BOUND);
        let grandchild_ended = !grandchild.running();
        let child_ended = !child.running();
        let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
        let status = status.unwrap_or_else(|| {
            panic!("case {case} ({mode}): the runner did not exit within {BOUND:?}\n{stderr}")
        });
        assert!(
            grandchild_ended,
            "case {case} ({mode}): grandchild {} was unsignalled when the runner returned\n{stderr}",
            grandchild.1
        );
        assert!(
            child_ended,
            "case {case} ({mode}): the command {} was unsignalled when the runner returned",
            child.1
        );
        assert_eq!(
            status.code(),
            Some(0),
            "case {case} ({mode}): the command's code passes through\n{stderr}"
        );
        assert!(
            !stderr.contains("windows-acl-run: "),
            "case {case} ({mode}): {stderr}"
        );
        assert!(
            entries(&root.0).is_empty(),
            "case {case} ({mode}): the private directory is removed once the tree is settled"
        );
    }
    eprintln!("windows-acl: {runs} pinned descendant cases ended with their runner");
    // Negative control: unconfined, the orphan survives its parent. The
    // control joins a Job this test owns before it starts the grandchild,
    // and the grandchild's handle is pinned the same way before the
    // control exits.
    let job = TestJob::new();
    let mut control = Owned::spawn(
        std::process::Command::new(exe())
            .args([PROBE_ARG, "orphan-pinned"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    job.assign(&control);
    let (mut stdin, grandchild, parent) = start_pinned(&mut control, String::new);
    stdin.write_all(b"opened\n").expect("release");
    drop(stdin);
    let status = control
        .wait_within(BOUND)
        .expect("the control exits within the bound");
    assert!(status.success(), "{status}");
    assert!(!parent.running(), "the control's handle is signalled");
    assert!(
        grandchild.running(),
        "the unconfined orphan must outlive its parent"
    );
    drop((grandchild, parent));
    drop(control);
    job.finish()
        .unwrap_or_else(|error| panic!("the control's Job was not reaped: {error}"));
}

/// Cancellation as a caller does it: the runner owned by
/// `bake_process::OwnedProcess`, as a tool runs a confined command, killed
/// while its command and an orphaned grandchild both run. The owner's
/// `TreeState::Gone` must come only once both, members of the runner's own
/// Job nested in the owner's, are signalled: checked at once on handles the
/// test pinned. Repeated [`descendant_runs`] times, alternating modes. The
/// killed runner cannot clean up, so its private directory lands under a
/// root this test owns.
fn an_owned_runner_killed_mid_run_settles_its_tree() {
    use bake_process::{OwnedProcess, ProcessSpec, TreeState};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let (workspace, _outside) = workspace_and_outside();
    let runs = descendant_runs();
    for case in 0..runs {
        let mode = if case % 2 == 0 {
            "workspace-write"
        } else {
            "read-only"
        };
        let root = Scratch::outside("acl-cancel-root");
        let spec = ProcessSpec {
            program: exe().into(),
            args: runner_argv(&workspace, &root, mode, &["orphan-pinned"])
                .into_iter()
                .map(OsString::from)
                .collect(),
            cwd: Some(workspace.0.clone()),
            env: None,
            stdin: true,
        };
        runtime().block_on(async {
            let mut process = tokio::time::timeout(BOUND, OwnedProcess::spawn(&spec, None))
                .await
                .expect("spawn within the bound")
                .expect("spawn the runner");
            let mut stdin = process.take_stdin().expect("piped stdin");
            let mut stdout = tokio::io::BufReader::new(process.take_stdout().expect("stdout"));
            let mut stderr = process.take_stderr().expect("stderr");
            tokio::time::timeout(BOUND, async {
                stdin.write_all(b"go\n").await?;
                stdin.flush().await
            })
            .await
            .expect("go within the bound")
            .expect("go");
            let mut line = String::new();
            let read =
                tokio::time::timeout(BOUND, (&mut stdout).take(LINE_CAP).read_line(&mut line))
                    .await
                    .expect("a handshake within the bound")
                    .expect("read the handshake");
            if read == 0 {
                // The command never reported: show why, then fail.
                process.kill();
                let settled = tokio::time::timeout(BOUND, process.finish()).await;
                let mut detail = Vec::new();
                let _ = tokio::time::timeout(
                    BOUND,
                    (&mut stderr).take(64 * 1024).read_to_end(&mut detail),
                )
                .await;
                panic!(
                    "case {case} ({mode}): no handshake; {settled:?}\n{}",
                    String::from_utf8_lossy(&detail)
                );
            }
            let (grandchild, child) = open_pinned(&line);
            process.kill();
            let settled = tokio::time::timeout(BOUND, process.finish())
                .await
                .unwrap_or_else(|_| panic!("case {case} ({mode}): no settle within {BOUND:?}"))
                .expect("finish");
            let grandchild_ended = !grandchild.running();
            let child_ended = !child.running();
            assert_eq!(settled.tree, TreeState::Gone, "case {case} ({mode})");
            assert!(
                grandchild_ended,
                "case {case} ({mode}): grandchild {} was unsignalled when the owner reported Gone",
                grandchild.1
            );
            assert!(
                child_ended,
                "case {case} ({mode}): the command {} was unsignalled when the owner reported Gone",
                child.1
            );
        });
    }
    eprintln!("windows-acl: {runs} owned runners killed mid-run settled with their trees");
}

fn a_killed_runner_takes_its_tree() {
    let (workspace, _outside) = workspace_and_outside();
    // A killed runner cannot clean up: its private directory lands under a
    // temp root this test owns, never the user's.
    let root = Scratch::outside("acl-killed-root");
    let mut runner = Owned::spawn(
        std::process::Command::new(exe())
            .arg(RUNNER_ARG)
            .args(["--workspace", &text(&workspace.0), "--temp", &text(&root.0)])
            .args(["--mode", "workspace-write", "--"])
            .args(probe(&["orphan-and-park"]))
            .current_dir(&workspace.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    let line = runner.first_line();
    let mut words = line.split_whitespace();
    let (Some("grandchild"), Some(grandchild), Some("child"), Some(direct)) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        panic!("handshake {line:?}");
    };
    let grandchild = OwnedPid::open(grandchild.parse().expect("pid"));
    let direct = OwnedPid::open(direct.parse().expect("pid"));
    assert!(grandchild.running() && direct.running());
    runner.0.kill().expect("kill the runner");
    runner
        .wait_within(BOUND)
        .expect("the runner exits within the bound");
    // The runner's last Job handle closed with it, which terminates the
    // tree; the kernel finishes each exit within the bound.
    assert!(direct.exited_within(BOUND), "the child outlived its runner");
    assert!(
        grandchild.exited_within(BOUND),
        "the grandchild outlived its runner"
    );
    // The documented residue: one private directory, removed with `root`.
    assert_eq!(entries(&root.0).len(), 1);
}

fn junction_escapes_are_denied() {
    let (workspace, outside) = workspace_and_outside();
    let victim = outside.write("victim.txt", "keep");
    // A junction that exists when the first grant propagates.
    let link = workspace.join("link");
    mklink_junction(&link, &outside.0);
    let policy = ww(&workspace);
    let run = confined(
        &policy,
        &workspace.0,
        &["write", &text(&link.join("new.txt")), "x"],
    );
    assert_denied(&run, "write through a pre-existing junction");
    let run = confined(
        &policy,
        &workspace.0,
        &["write", &text(&link.join("victim.txt")), "x"],
    );
    assert_denied(&run, "overwrite through a pre-existing junction");
    // Propagation did not follow the junction.
    let sid = ws_sid(&workspace);
    assert_eq!(allow_aces_naming(&outside.0, &sid), Ok(0));
    assert_eq!(allow_aces_naming(&victim, &sid), Ok(0));
    // A junction the confined command makes itself.
    let made = workspace.join("made");
    let run = run_argv(
        &provider(),
        &["cmd", "/c", "mklink", "/J", &text(&made), &text(&outside.0)],
        &policy,
        &workspace.0,
    );
    assert_eq!(
        run.code,
        Some(0),
        "a junction inside the workspace: {}",
        run.stderr
    );
    let run = confined(
        &policy,
        &workspace.0,
        &["write", &text(&made.join("new2.txt")), "x"],
    );
    assert_denied(&run, "write through a junction the command made");
    assert!(!outside.join("new.txt").exists());
    assert!(!outside.join("new2.txt").exists());
    assert_eq!(std::fs::read_to_string(&victim).expect("victim"), "keep");
    // Remove the junctions as links, never through them.
    std::fs::remove_dir(&link).expect("unlink");
    std::fs::remove_dir(&made).expect("unlink");
    assert!(victim.exists());
}

fn hard_links_are_the_documented_partial_boundary() {
    let (workspace, outside) = workspace_and_outside();
    let target = outside.write("linked.txt", "keep");
    let other = outside.write("other.txt", "keep");
    // A link the confined command makes to an outside file is denied:
    // creating it needs write access to the target.
    let policy = ww(&workspace);
    let run = confined(
        &policy,
        &workspace.0,
        &[
            "hardlink",
            &text(&other),
            &text(&workspace.join("alias2.txt")),
        ],
    );
    assert_denied(&run, "hard-link an outside file into the workspace");
    assert!(!workspace.join("alias2.txt").exists());
    // A link that exists when the first grant propagates aliases the file
    // object: the ACE lands on its one security descriptor, so the outside
    // path becomes writable. This is the boundary `Partial` reports.
    let fresh = user_scratch("acl-ws-hl");
    std::fs::hard_link(&target, fresh.join("alias.txt")).expect("hard link");
    let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &fresh.0);
    let run = confined(&policy, &fresh.0, &["write", &text(&target), "changed"]);
    assert_ok(&run, "the documented hard-link alias");
    assert_eq!(std::fs::read_to_string(&target).expect("target"), "changed");
    assert!(allow_aces_naming(&target, &workspace_write_sid(&text(&fresh.0))).expect("aces") >= 1);
    assert_eq!(std::fs::read_to_string(&other).expect("other"), "keep");
}

fn a_failed_spawn_is_a_runner_failure_and_cleans_up() {
    let (workspace, _outside) = workspace_and_outside();
    let missing = workspace.join("no-such-program.exe");
    let run = run_argv(
        &provider(),
        &[&text(&missing)],
        &ww(&workspace),
        &workspace.0,
    );
    assert_eq!(
        run.code,
        Some(windows_acl::RUNNER_FAILURE_EXIT),
        "{}",
        run.stderr
    );
    let failure = run.runner_failure().expect("a runner failure");
    assert!(failure.contains("CreateProcessAsUserW"), "{failure}");
    let root = Scratch::outside("acl-failed-root");
    let output = runner(
        &[
            "--workspace",
            &text(&workspace.0),
            "--temp",
            &text(&root.0),
            "--mode",
            "workspace-write",
        ],
        &[text(&missing)],
    );
    assert_eq!(output.status.code(), Some(windows_acl::RUNNER_FAILURE_EXIT));
    assert!(
        entries(&root.0).is_empty(),
        "the private directory must be removed after a failed spawn"
    );
}

fn runner_cli_failures_grant_nothing() {
    let (workspace, outside) = workspace_and_outside();
    let sid = ws_sid(&workspace);
    let command = probe(&["write", &text(&workspace.join("x.txt")), "x"]);
    let inner = workspace.join("tmp");
    std::fs::create_dir(&inner).expect("inner temp");
    let cases: Vec<(Vec<String>, &str)> = vec![
        (
            vec![
                "--workspace".into(),
                text(&workspace.0),
                "--temp".into(),
                text(&inner),
                "--mode".into(),
                "workspace-write".into(),
            ],
            "temp root must be outside the workspace",
        ),
        (
            vec![
                "--workspace".into(),
                text(&workspace.join("missing")),
                "--temp".into(),
                text(&outside.0),
                "--mode".into(),
                "workspace-write".into(),
            ],
            "is not an existing directory",
        ),
        (
            vec![
                "--workspace".into(),
                text(&workspace.0),
                "--temp".into(),
                text(&outside.0),
                "--mode".into(),
                "full".into(),
            ],
            "unknown mode: full",
        ),
        (
            vec![
                "--workspace".into(),
                text(&workspace.0),
                "--mode".into(),
                "read-only".into(),
            ],
            "missing --temp",
        ),
    ];
    for (args, detail) in cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = runner(&refs, &command);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(windows_acl::RUNNER_FAILURE_EXIT),
            "{stderr}"
        );
        assert!(
            stderr.contains("windows-acl-run: ") && stderr.contains(detail),
            "{stderr}"
        );
    }
    assert!(
        !workspace.join("x.txt").exists(),
        "the command must never run"
    );
    assert_eq!(allow_aces_naming(&workspace.0, &sid), Ok(0));
    assert!(entries(&outside.0).is_empty());
}

fn repeated_runs_reuse_one_standing_grant() {
    let (workspace, _outside) = workspace_and_outside();
    let policy = ww(&workspace);
    let sid = ws_sid(&workspace);
    for index in 0..3 {
        let path = workspace.join(&format!("run{index}.txt"));
        assert_ok(
            &confined(&policy, &workspace.0, &["write", &text(&path), "x"]),
            "a repeated run",
        );
        assert_eq!(
            allow_aces_naming(&workspace.0, &sid),
            Ok(1),
            "one standing ACE after run {index}"
        );
    }
}

fn read_only_leaves_a_standing_grant_inert() {
    let (workspace, _outside) = workspace_and_outside();
    assert_ok(
        &confined(
            &ww(&workspace),
            &workspace.0,
            &["write", &text(&workspace.join("a.txt")), "a"],
        ),
        "workspace-write first",
    );
    assert_eq!(allow_aces_naming(&workspace.0, &ws_sid(&workspace)), Ok(1));
    assert_denied(
        &confined(
            &ro(&workspace),
            &workspace.0,
            &["write", &text(&workspace.join("b.txt")), "b"],
        ),
        "read-only with a standing ACE",
    );
    assert!(!workspace.join("b.txt").exists());
}

fn exit_codes_and_output_pass_through() {
    let (workspace, _outside) = workspace_and_outside();
    for code in ["0", "42", "127"] {
        let run = confined(&ww(&workspace), &workspace.0, &["exit", code]);
        assert_eq!(run.code, Some(code.parse().expect("code")));
        assert_eq!(
            run.runner_failure(),
            None,
            "a command's own 127 is not a runner failure"
        );
    }
    let run = confined(
        &ro(&workspace),
        &workspace.0,
        &["echo", "to stdout", "to stderr"],
    );
    assert_ok(&run, "echo");
    assert_eq!(run.stdout.trim(), "to stdout");
    assert_eq!(run.stderr.trim(), "to stderr");
}

/// The counterexample behind [`the_default_provider_refuses`], on private
/// roots whose DACLs grant the user full access: under a write-restricted
/// token `DELETE` and `FILE_DELETE_CHILD` pass while every write access
/// fails, so each deletion primitive succeeds in both modes, and under
/// `workspace-write` an outside file moves into the workspace. Each case
/// prints its exact token, DACL, and result, and the disk is checked
/// against every report. Should a Windows release start denying these, this
/// fails, and the runner's exclusion from the default chain is due for
/// review.
fn deletion_escapes_the_write_restricted_token() {
    let (workspace, outside) = workspace_and_outside();
    let cases = [
        ("read-only, in the workspace", ro(&workspace), &workspace),
        ("workspace-write, outside", ww(&workspace), &outside),
    ];
    for (label, policy, dir) in cases {
        let token = confined(&policy, &workspace.0, &["token"]);
        assert_ok(&token, "the token report");
        eprintln!("deletion[{label}] token: {}", token.stdout.trim());
        assert!(token.stdout.contains("restricted=true"), "{}", token.stdout);
        eprintln!(
            "deletion[{label}] parent {}: {}",
            dir.0.display(),
            win32::sddl(&dir.0)
        );
        let mut index = 0;
        let mut fresh = |stem: &str| {
            index += 1;
            dir.write(&format!("{stem}-{index}.txt"), "keep")
        };
        let sample = fresh("acl");
        eprintln!("deletion[{label}] file: {}", win32::sddl(&sample));
        for (what, mask, path, granted) in [
            ("DELETE on the file", "0x10000", &sample, true),
            ("READ_CONTROL on the file", "0x20000", &sample, true),
            ("FILE_WRITE_DATA on the file", "0x2", &sample, false),
            ("FILE_WRITE_ATTRIBUTES on the file", "0x100", &sample, false),
            ("WRITE_DAC on the file", "0x40000", &sample, false),
            ("FILE_DELETE_CHILD on the parent", "0x40", &dir.0, true),
            ("FILE_ADD_FILE on the parent", "0x2", &dir.0, false),
        ] {
            let run = confined(&policy, &workspace.0, &["open", mask, &text(path)]);
            eprintln!(
                "deletion[{label}] open {what} ({mask}): exit={:?} {}",
                run.code,
                run.stderr.trim()
            );
            if granted {
                assert_ok(&run, what);
            } else {
                assert_denied(&run, what);
            }
        }
        for method in ["DeleteFileW", "disposition", "posix", "std"] {
            let victim = fresh(method);
            let run = confined(
                &policy,
                &workspace.0,
                &["win32-delete", method, &text(&victim)],
            );
            eprintln!(
                "deletion[{label}] delete {method}: exit={:?} exists_after={}",
                run.code,
                victim.exists()
            );
            assert_ok(&run, method);
            assert!(
                !victim.exists(),
                "{label} {method}: the report must match the disk"
            );
        }
        let victim = fresh("powershell");
        let script = format!(
            "Remove-Item -LiteralPath '{}' -ErrorAction Stop",
            text(&victim)
        );
        let run = run_argv(
            &provider(),
            &[
                "powershell",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ],
            &policy,
            &workspace.0,
        );
        eprintln!(
            "deletion[{label}] delete Remove-Item: exit={:?} exists_after={}",
            run.code,
            victim.exists()
        );
        assert_ok(&run, "Remove-Item");
        assert!(
            !victim.exists(),
            "{label} Remove-Item: the report must match the disk"
        );
        let victim = fresh("moved");
        let destination = workspace.join(&format!(
            "in-{}",
            victim
                .file_name()
                .and_then(|name| name.to_str())
                .expect("name")
        ));
        let run = confined(
            &policy,
            &workspace.0,
            &["rename", &text(&victim), &text(&destination)],
        );
        let (gone, arrived) = (!victim.exists(), destination.exists());
        eprintln!(
            "deletion[{label}] rename into the workspace: exit={:?} source_gone={gone} arrived={arrived}",
            run.code
        );
        if policy.mode == SandboxMode::WorkspaceWrite {
            assert_ok(&run, "move an outside file into the workspace");
            assert!(
                gone && arrived,
                "{label} rename: the report must match the disk"
            );
        } else {
            // The destination directory needs FILE_ADD_FILE.
            assert_denied(&run, "rename within a read-only workspace");
            assert!(
                !gone && !arrived,
                "{label} rename: the report must match the disk"
            );
        }
    }
}

/// The raw Win32 primitives the deletion diagnosis compares, so each
/// outcome names one exact call rather than a library's fallback chain.
mod win32 {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetTokenInformation, IsTokenRestricted,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER,
        TokenRestrictedSids, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, DELETE, DeleteFileW, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo, FileDispositionInfoEx,
        OPEN_EXISTING, SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text).encode_wide().chain([0]).collect()
    }

    fn check(ok: i32) -> std::io::Result<()> {
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn open_with(mask: u32, path: &str) -> std::io::Result<OwnedHandle> {
        let units = wide(path);
        // SAFETY: `units` is NUL-terminated; no security attributes or template.
        let raw = unsafe {
            CreateFileW(
                units.as_ptr(),
                mask,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a fresh handle owned here.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    pub fn open(mask: &str, path: &str) -> std::io::Result<()> {
        let mask = u32::from_str_radix(mask.trim_start_matches("0x"), 16).expect("hex access mask");
        open_with(mask, path).map(drop)
    }

    pub fn delete(method: &str, path: &str) -> std::io::Result<()> {
        match method {
            // The first call `std::fs::remove_file` makes.
            "DeleteFileW" => {
                let units = wide(path);
                // SAFETY: `units` is NUL-terminated.
                check(unsafe { DeleteFileW(units.as_ptr()) })
            }
            // Open for DELETE, then the classic delete disposition.
            "disposition" => {
                let handle = open_with(DELETE, path)?;
                let info = FILE_DISPOSITION_INFO { DeleteFile: true };
                // SAFETY: a live handle and a struct of the size passed.
                check(unsafe {
                    SetFileInformationByHandle(
                        handle.as_raw_handle(),
                        FileDispositionInfo,
                        std::ptr::addr_of!(info).cast::<c_void>(),
                        size_of::<FILE_DISPOSITION_INFO>() as u32,
                    )
                })
            }
            // Open for DELETE, then the POSIX disposition std falls back to.
            "posix" => {
                let handle = open_with(DELETE, path)?;
                let info = FILE_DISPOSITION_INFO_EX {
                    Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
                };
                // SAFETY: a live handle and a struct of the size passed.
                check(unsafe {
                    SetFileInformationByHandle(
                        handle.as_raw_handle(),
                        FileDispositionInfoEx,
                        std::ptr::addr_of!(info).cast::<c_void>(),
                        size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
                    )
                })
            }
            "std" => std::fs::remove_file(path),
            other => panic!("unknown deletion method {other}"),
        }
    }

    pub(super) fn sid_text(sid: *mut c_void) -> String {
        let mut text = null_mut();
        // SAFETY: a valid SID; `text` receives a LocalAlloc'd string.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return format!("<{}>", std::io::Error::last_os_error());
        }
        // SAFETY: a NUL-terminated string from the call above.
        let value = unsafe {
            String::from_utf16_lossy(std::slice::from_raw_parts(
                text,
                (0..).take_while(|&i| *text.add(i) != 0).count(),
            ))
        };
        // SAFETY: allocated by the call above, freed once.
        unsafe { LocalFree(text.cast()) };
        value
    }

    fn information(token: &OwnedHandle, class: i32) -> std::io::Result<Vec<u64>> {
        let mut needed = 0u32;
        // SAFETY: a size query.
        unsafe { GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut needed) };
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: `buffer` holds at least `needed` bytes.
        check(unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                &mut needed,
            )
        })?;
        Ok(buffer)
    }

    fn process_token() -> std::io::Result<OwnedHandle> {
        let mut raw = null_mut();
        // SAFETY: the pseudo-handle needs no closing.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) })?;
        // SAFETY: a fresh handle owned here.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    fn token_user(token: &OwnedHandle) -> std::io::Result<String> {
        let user = information(token, TokenUser)?;
        // SAFETY: the buffer holds a TOKEN_USER.
        Ok(sid_text(unsafe {
            (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid
        }))
    }

    /// This process's user SID.
    pub fn user_sid() -> std::io::Result<String> {
        token_user(&process_token()?)
    }

    /// This process's user SID, whether its token is restricted, and its
    /// restricting SIDs.
    pub fn token_report() -> std::io::Result<String> {
        let token = process_token()?;
        let user = token_user(&token)?;
        // SAFETY: a live token handle.
        let restricted = unsafe { IsTokenRestricted(token.as_raw_handle()) } != 0;
        let groups = information(&token, TokenRestrictedSids)?;
        let base = groups.as_ptr().cast::<TOKEN_GROUPS>();
        // SAFETY: the buffer holds a TOKEN_GROUPS whose array lies in it.
        let sids: Vec<String> = unsafe {
            let count = (*base).GroupCount as usize;
            std::slice::from_raw_parts((*base).Groups.as_ptr(), count)
                .iter()
                .map(|group| sid_text(group.Sid))
                .collect()
        };
        Ok(format!(
            "user={user} restricted={restricted} restricting=[{}]",
            sids.join(",")
        ))
    }

    /// `path`'s owner and DACL as SDDL, with exact access masks.
    pub fn sddl(path: &std::path::Path) -> String {
        let units: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        // SAFETY: NUL-terminated name; the descriptor is LocalAlloc'd.
        let status = unsafe {
            GetNamedSecurityInfoW(
                units.as_ptr(),
                SE_FILE_OBJECT,
                info,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return format!("<GetNamedSecurityInfoW {status}>");
        }
        let mut text = null_mut();
        // SAFETY: a valid descriptor; `text` receives a LocalAlloc'd string.
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                info,
                &mut text,
                null_mut(),
            )
        };
        let value = if ok == 0 {
            format!("<{}>", std::io::Error::last_os_error())
        } else {
            // SAFETY: a NUL-terminated string from the call above.
            let value = unsafe {
                String::from_utf16_lossy(std::slice::from_raw_parts(
                    text,
                    (0..).take_while(|&i| *text.add(i) != 0).count(),
                ))
            };
            // SAFETY: allocated by the call above, freed once.
            unsafe { LocalFree(text.cast()) };
            value
        };
        // SAFETY: allocated by GetNamedSecurityInfoW, freed once.
        unsafe { LocalFree(descriptor) };
        value
    }
}
