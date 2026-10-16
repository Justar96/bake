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
//! per-target temporary directory; the runner's own private temp
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
//! it on drop, a panic included, and every handshake is bounded.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use bake_sandbox::backends::windows_acl::{
    self, HELPER_ARG as RUNNER_ARG, PRIVATE_TEMP_PREFIX, allow_aces_naming, workspace_write_sid,
};
use bake_sandbox::backends::{LocalProvider, Runner};
use bake_sandbox::{Enforcement, SandboxMode, SandboxPolicy, SandboxProvider, default_provider};

use super::{Run, Scratch, harness, run_argv, runtime};

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
    let result: std::io::Result<i32> = match refs.as_slice() {
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
    let child = std::process::Command::new(std::env::current_exe()?)
        .args([PROBE_ARG, "park"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child.id())
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
    (Scratch::outside("acl-ws"), Scratch::outside("acl-out"))
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

/// A child this test started. Dropped, a panic included, it is killed and
/// awaited within [`BOUND`].
struct Owned(std::process::Child);

impl Owned {
    fn spawn(command: &mut std::process::Command) -> Self {
        Self(command.spawn().expect("spawn"))
    }

    /// The exit status, if the child exits within `budget`.
    fn wait_within(&mut self, budget: Duration) -> Option<std::process::ExitStatus> {
        use std::os::windows::io::AsRawHandle;
        signaled_within(self.0.as_raw_handle(), budget).then(|| self.0.wait().expect("reap"))
    }

    /// The first stdout line, within [`BOUND`]. The reader thread ends at
    /// that line or at EOF, whichever comes first.
    fn first_line(&mut self) -> String {
        let stdout = self.0.stdout.take().expect("piped stdout");
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = send.send(line);
        });
        let line = receive
            .recv_timeout(BOUND)
            .expect("a handshake line within the bound");
        assert!(
            !line.is_empty(),
            "the process closed stdout before its handshake"
        );
        line
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
    }
}

/// Whether the process behind `handle` (opened with
/// `PROCESS_QUERY_LIMITED_INFORMATION`) runs this test binary.
fn is_this_binary(handle: std::os::windows::io::RawHandle) -> bool {
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
    let image = PathBuf::from(String::from_utf16_lossy(&name[..size as usize]));
    bake_sandbox::canonical_path(&image)
        == bake_sandbox::canonical_path(&std::env::current_exe().expect("exe"))
}

/// A descendant this test learned of from a handshake, held by handle so
/// its pid cannot be reused while held. Dropped, it is terminated and
/// awaited within [`BOUND`].
struct OwnedPid(std::os::windows::io::OwnedHandle, u32);

impl OwnedPid {
    /// Opens `pid`, which must still be this test binary.
    fn open(pid: u32) -> Self {
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
            is_this_binary(handle.as_raw_handle()),
            "pid {pid} is not this test binary"
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

/// A Job this test puts an unconfined negative control in before the
/// control starts any descendant. Dropped, it terminates every member and
/// awaits each within [`BOUND`].
struct TestJob(std::os::windows::io::OwnedHandle);

impl TestJob {
    fn new() -> Self {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
        // SAFETY: null attributes (not inheritable) and no name.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(
            !raw.is_null(),
            "CreateJobObjectW: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: a fresh handle owned here.
        Self(unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    fn assign(&self, child: &Owned) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: two live handles.
        let ok =
            unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.0.as_raw_handle()) };
        assert_ne!(
            ok,
            0,
            "AssignProcessToJobObject: {}",
            std::io::Error::last_os_error()
        );
    }

    fn members(&self) -> Vec<u32> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList, QueryInformationJobObject,
        };
        let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList)
            / size_of::<usize>();
        let mut buffer = vec![0usize; header + 256];
        // SAFETY: `buffer` holds the bytes passed, aligned for the struct.
        unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<usize>()) as u32,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: the header was written (or is still zero).
        let listed = unsafe {
            (*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()).NumberOfProcessIdsInList
        };
        buffer[header..][..(listed as usize).min(256)]
            .iter()
            .map(|&id| id as u32)
            .collect()
    }
}

impl Drop for TestJob {
    fn drop(&mut self) {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        // SAFETY: a live Job handle.
        unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
        let deadline = std::time::Instant::now() + BOUND;
        for pid in self.members() {
            // SAFETY: plain call; null means the process is gone.
            let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            if raw.is_null() {
                continue;
            }
            // SAFETY: a fresh handle owned here.
            let process = unsafe { OwnedHandle::from_raw_handle(raw) };
            signaled_within(
                process.as_raw_handle(),
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
        if !std::thread::panicking() {
            assert!(
                self.members().is_empty(),
                "the test Job still holds processes"
            );
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

fn descendants_end_with_the_command() {
    let (workspace, _outside) = workspace_and_outside();
    let run = confined(&ww(&workspace), &workspace.0, &["orphan"]);
    assert_ok(&run, "orphan a grandchild");
    let pid: u32 = run
        .stdout
        .trim()
        .strip_prefix("grandchild ")
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("grandchild pid in {:?}", run.stdout));
    // The runner exits only after its Job is empty, so the grandchild is
    // gone (or its pid already names another program).
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: plain call; null means no such process.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if !raw.is_null() {
            // SAFETY: a fresh handle owned here.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            assert!(
                signaled_within(handle.as_raw_handle(), Duration::ZERO)
                    || !is_this_binary(handle.as_raw_handle()),
                "grandchild {pid} outlived the run"
            );
        }
    }
    // Negative control: unconfined, the orphan survives its parent. The
    // control joins a Job this test owns before it starts the grandchild,
    // and the grandchild's inherited stdout is never awaited to EOF.
    let job = TestJob::new();
    let mut control = Owned::spawn(
        std::process::Command::new(exe())
            .args([PROBE_ARG, "orphan"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    job.assign(&control);
    control
        .0
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"go\n")
        .expect("go");
    let line = control.first_line();
    let grandchild = OwnedPid::open(
        line.trim()
            .strip_prefix("grandchild ")
            .and_then(|pid| pid.parse().ok())
            .expect("control pid"),
    );
    let status = control
        .wait_within(BOUND)
        .expect("the control exits within the bound");
    assert!(status.success(), "{status}");
    assert!(
        grandchild.running(),
        "the unconfined orphan must outlive its parent"
    );
    drop(grandchild);
    drop(control);
    drop(job);
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
    let fresh = Scratch::outside("acl-ws-hl");
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

    fn sid_text(sid: *mut c_void) -> String {
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

    /// This process's user SID, whether its token is restricted, and its
    /// restricting SIDs.
    pub fn token_report() -> std::io::Result<String> {
        let mut raw = null_mut();
        // SAFETY: the pseudo-handle needs no closing.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) })?;
        // SAFETY: a fresh handle owned here.
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        let user = information(&token, TokenUser)?;
        // SAFETY: the buffer holds a TOKEN_USER.
        let user = sid_text(unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid });
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
