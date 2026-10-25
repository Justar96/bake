//! Bake: an owned process tree on Windows (`bake-process`). The
//! command is born in a kill-on-close job; a normal exit clears that limit
//! before the job's handle closes, so an intentionally started background
//! process keeps running, as Pi leaves it; before that detach, the death of
//! the process that owns the job ends the tree; an abort terminates the job
//! and returns once it is empty. A job a descendant creates is nested in
//! the call's, so its processes are ended and awaited with the rest, and no
//! descendant breaks away from the call's job.
//!
//! Each case starts its background process with PowerShell's
//! `Start-Process`, which writes its pid to a file in the case's own
//! directory. The test opens the pid at once and keeps the handle, so later
//! checks name that process and no other.
//!
//! Fixture ownership: every wait is bounded by [`BOUND`]. The call's task
//! ([`Call`]), the helper process, and the background process
//! ([`Background`]) stay owned through a timeout or a readiness failure;
//! each case records its observations, then stops and awaits everything it
//! owns, and only then reports, through [`verdict`], which names assertion
//! failures and cleanup failures separately. A cleanup that does not
//! observe its process or task end is a failure, never a success. The drop
//! impls are only a last resort for a panic inside the fixture itself.
#![cfg(windows)]

use std::ffi::OsString;
use std::future::Future;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bake_ai::AbortController;
use bake_coding_agent::exec::{ExecOptions, ExecResult, exec_command};
use bake_coding_agent::owned_work::WorkGate;
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

/// The bound of every wait in this file.
const BOUND: Duration = Duration::from_secs(30);
const HELPER: &str = "BAKE_PROCESS_WINDOWS_HELPER";
/// Set, to the case's directory, in `nested_case`'s environment.
const NESTED: &str = "BAKE_PROCESS_WINDOWS_NESTED";

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).expect("random");
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

/// What a case observed (`Err` is a failed assertion) and what its cleanup
/// established (`Err` is a cleanup failure). Panics, naming each kind
/// apart, unless both are clean.
fn verdict(observed: Vec<Result<(), String>>, cleanup: Vec<Result<(), String>>) {
    let failed: Vec<String> = observed.into_iter().filter_map(Result::err).collect();
    let unclean: Vec<String> = cleanup.into_iter().filter_map(Result::err).collect();
    assert!(
        failed.is_empty() && unclean.is_empty(),
        "assertion failures: {failed:?}; cleanup failures: {unclean:?}"
    );
}

fn check(holds: bool, what: &str) -> Result<(), String> {
    if holds { Ok(()) } else { Err(what.to_owned()) }
}

/// Whether the process behind `handle` has exited, at once.
fn exited(handle: &OwnedHandle) -> bool {
    // SAFETY: a live handle opened with SYNCHRONIZE.
    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) == WAIT_OBJECT_0 }
}

/// Polls `handle` until its process has exited or `bound` has passed.
async fn exited_within(handle: &OwnedHandle, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        if exited(handle) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A background process this test holds by handle. [`Self::stop`] ends it
/// and observes the exit through that same handle before closing it.
struct Background(Option<OwnedHandle>);

impl Background {
    /// Waits, at most [`BOUND`], for `file` to hold a pid, then opens that
    /// process. An error leaves nothing to clean up.
    async fn open(file: &Path) -> Result<Self, String> {
        let deadline = Instant::now() + BOUND;
        let pid: u32 = loop {
            if let Some(pid) = std::fs::read_to_string(file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            if Instant::now() >= deadline {
                return Err("no background pid was written".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        Self::open_pid(pid)
    }

    /// Opens process `pid`. An error leaves nothing to clean up.
    fn open_pid(pid: u32) -> Result<Self, String> {
        // SAFETY: plain call; a null handle means the open failed.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(format!(
                "background process {pid} could not be opened: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: a fresh handle owned here.
        Ok(Self(Some(unsafe { OwnedHandle::from_raw_handle(handle) })))
    }

    fn handle(&self) -> &OwnedHandle {
        self.0.as_ref().expect("held until stopped")
    }

    fn running(&self) -> bool {
        // SAFETY: a live handle opened with SYNCHRONIZE.
        unsafe { WaitForSingleObject(self.handle().as_raw_handle(), 0) == WAIT_TIMEOUT }
    }

    async fn ended_within(&self, bound: Duration) -> bool {
        exited_within(self.handle(), bound).await
    }

    /// Terminates the process unless it has exited, then waits, at most
    /// [`BOUND`], to observe its exit through the retained handle. The
    /// termination call's result is not read; only the wait counts.
    async fn stop(mut self) -> Result<(), String> {
        let Some(handle) = self.0.take() else {
            return Ok(());
        };
        if !exited(&handle) {
            // SAFETY: a live handle with terminate access.
            unsafe {
                TerminateProcess(handle.as_raw_handle(), 1);
            }
        }
        if exited_within(&handle, BOUND).await {
            Ok(())
        } else {
            Err("the background process did not exit after termination".into())
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        // Only reached when the fixture itself panicked before `stop`.
        if let Some(handle) = self.0.take() {
            // SAFETY: a live handle with terminate and synchronize access.
            let waited = unsafe {
                TerminateProcess(handle.as_raw_handle(), 1);
                WaitForSingleObject(handle.as_raw_handle(), 10_000)
            };
            if waited != WAIT_OBJECT_0 {
                eprintln!("cleanup failure: a background process outlived its fixture");
            }
        }
    }
}

/// The task running an `exec_command` call, owned until stopped.
struct Call(Option<tokio::task::JoinHandle<ExecResult>>);

impl Call {
    fn spawn(call: impl Future<Output = ExecResult> + Send + 'static) -> Self {
        Self(Some(tokio::spawn(call)))
    }

    /// The call's result, waiting at most `bound`. On a timeout the task
    /// stays owned, for [`Self::stop`].
    async fn join(&mut self, bound: Duration) -> Result<ExecResult, String> {
        let Some(task) = self.0.as_mut() else {
            return Err("the call was already joined".into());
        };
        match tokio::time::timeout(bound, task).await {
            Err(_) => Err("the call did not return within the bound".into()),
            Ok(joined) => {
                self.0 = None;
                joined.map_err(|error| format!("the call's task failed: {error}"))
            }
        }
    }

    /// Aborts a call still running and awaits its task, at most [`BOUND`];
    /// the call's own drop then hands its process to its owner.
    async fn stop(mut self) -> Result<(), String> {
        let Some(task) = self.0.take() else {
            return Ok(());
        };
        task.abort();
        match tokio::time::timeout(BOUND, task).await {
            Ok(_) => Ok(()),
            Err(_) => Err("the aborted call's task did not end".into()),
        }
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        // Only reached when the fixture itself panicked before `stop`.
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// Closes `owner`, waiting at most [`BOUND`] for its processes.
async fn close(owner: &WorkGate) -> Result<(), String> {
    tokio::time::timeout(BOUND, owner.close())
        .await
        .map_err(|_| "the owner did not drain".to_owned())
}

/// `path` as a single-quoted PowerShell string.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

/// PowerShell that first writes its own pid to `leader`, if given, then
/// starts a hidden `ping`, writes that pid to `file`, and runs `then`.
fn powershell(leader: Option<&Path>, file: &Path, then: &str) -> Vec<OsString> {
    let leader = leader
        .map(|leader| {
            format!(
                "Set-Content -NoNewline -LiteralPath {} -Value $PID; ",
                quoted(leader)
            )
        })
        .unwrap_or_default();
    vec![
        "-NoProfile".into(),
        "-Command".into(),
        format!(
            "{leader}$p = Start-Process -PassThru -WindowStyle Hidden ping -ArgumentList '-n','600','127.0.0.1'; \
             Set-Content -NoNewline -LiteralPath {} -Value $p.Id; {then}",
            quoted(file)
        )
        .into(),
    ]
}

/// Closes `owner` again if `first` failed, after the case has stopped what
/// could hold it, so no settling task outlives the case. `first` is kept
/// as reported; the retry is pushed as a cleanup result of its own.
async fn redrain(
    owner: &WorkGate,
    first: &Result<(), String>,
    cleanup: &mut Vec<Result<(), String>>,
) {
    if first.is_err() {
        cleanup.push(
            close(owner)
                .await
                .map_err(|error| format!("retry: {error}")),
        );
    }
}

fn exec(args: Vec<OsString>, cwd: PathBuf, options: ExecOptions) -> Call {
    Call::spawn(async move { exec_command("powershell".as_ref(), &args, &cwd, options).await })
}

#[test]
fn powershell_paths_are_quoted() {
    assert_eq!(
        quoted(Path::new(r"C:\Users\O'Brien\pid")),
        r"'C:\Users\O''Brien\pid'"
    );
}

// A normal exit detaches its job: the background process outlives the call
// and the owner's close, and the call's result is the normal one. The
// command exits only once the test has opened the background process and
// created the `go` file, so the handle names it before any job closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_normal_exit_leaves_its_background_running() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let file = dir.0.join("pid");
    let go = dir.0.join("go");
    let owner = WorkGate::default();
    let then = format!(
        "while (-not (Test-Path -LiteralPath {})) {{ Start-Sleep -Milliseconds 20 }}; exit 0",
        quoted(&go)
    );
    let mut call = exec(
        powershell(None, &file, &then),
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let background = Background::open(&file).await;
    let mut observed = Vec::new();
    // Released only once the background is held: after a readiness failure
    // the command never exits normally, and the call is stopped instead,
    // which kills its still kill-on-close job.
    if background.is_ok()
        && let Err(error) = std::fs::write(&go, b"")
    {
        observed.push(Err(format!("the go file could not be written: {error}")));
    }
    let joined = if background.is_ok() {
        call.join(BOUND).await
    } else {
        Err("not joined: the background was not opened".to_owned())
    };
    match joined {
        Ok(result) => {
            observed.push(check(result.code == 0, &format!("exit code: {result:?}")));
            observed.push(check(
                !result.killed && !result.aborted,
                "the call was killed",
            ));
        }
        Err(error) => observed.push(Err(error)),
    }
    let mut cleanup = vec![call.stop().await];
    // With the call joined, this close is the observation; otherwise it
    // is cleanup of the stopped call.
    let closed = close(&owner).await;
    let first_close = closed.clone();
    match &background {
        Ok(background) => {
            observed.push(closed);
            // A job closed with kill-on-close ends its members shortly
            // after; a second's grace lets a broken detach show.
            observed.push(check(
                !background.ended_within(Duration::from_secs(1)).await && background.running(),
                "the normal exit's background was killed",
            ));
        }
        Err(error) => {
            observed.push(Err(error.clone()));
            cleanup.push(closed);
        }
    }
    if let Ok(background) = background {
        cleanup.push(background.stop().await);
    }
    redrain(&owner, &first_close, &mut cleanup).await;
    verdict(observed, cleanup);
}

// An abort terminates the job and returns once it has no active process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_ends_and_awaits_the_whole_job() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let file = dir.0.join("pid");
    let abort = AbortController::new();
    let owner = WorkGate::default();
    let mut call = exec(
        powershell(None, &file, "Start-Sleep -Seconds 600"),
        dir.0.clone(),
        ExecOptions {
            signal: Some(abort.signal()),
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let background = Background::open(&file).await;
    abort.abort();
    let mut observed = Vec::new();
    match call.join(BOUND).await {
        Ok(result) => observed.push(check(
            result.aborted && result.killed,
            &format!("not an aborted kill: {result:?}"),
        )),
        Err(error) => observed.push(Err(error)),
    }
    match &background {
        // The call returned only once the job was empty: already ended.
        Ok(background) => observed.push(check(
            background.ended_within(Duration::ZERO).await,
            "the call returned while a job member ran",
        )),
        Err(error) => observed.push(Err(error.clone())),
    }
    let stopped = call.stop().await;
    let first_close = close(&owner).await;
    let mut cleanup = vec![stopped, first_close.clone()];
    if let Ok(background) = background {
        cleanup.push(background.stop().await);
    }
    // With the job's members gone, the settling task can finish.
    redrain(&owner, &first_close, &mut cleanup).await;
    verdict(observed, cleanup);
}

// Before a normal exit detaches it, the job ends with the process that
// owns it: a helper (this binary's `helper_case`) runs a command that
// starts a background process and keeps running; the test terminates the
// helper and both the command (the job's leader) and the background
// process must end. The test holds both by handle before the kill, so it
// can stop them itself if they survive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owners_death_before_detach_ends_the_tree() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let file = dir.0.join("pid");
    let leader_file = dir.0.join("leader");
    let mut helper = tokio::process::Command::new(std::env::current_exe().expect("exe"))
        .args(["--exact", "helper_case", "--ignored", "--nocapture"])
        .env(HELPER, &file)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("helper");
    // The leader writes its pid before it starts the background, so once
    // the background is reported the leader's pid is there.
    let background = Background::open(&file).await;
    let leader = Background::open(&leader_file).await;
    // The helper's termination is the case's action, and its reaping its
    // cleanup: both happen whether or not the background was seen.
    let _ = helper.start_kill();
    let reaped = match tokio::time::timeout(BOUND, helper.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(format!("the helper could not be waited for: {error}")),
        Err(_) => Err("the terminated helper did not exit".to_owned()),
    };
    let mut observed = Vec::new();
    for (held, what) in [(&background, "background"), (&leader, "command")] {
        match held {
            Ok(held) => observed.push(check(
                held.ended_within(BOUND).await,
                &format!("the {what} outlived the process that owned its job"),
            )),
            Err(error) => observed.push(Err(format!("{what}: {error}"))),
        }
    }
    let mut cleanup = vec![reaped];
    for held in [background, leader].into_iter().flatten() {
        cleanup.push(held.stop().await);
    }
    verdict(observed, cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by the_owners_death_before_detach_ends_the_tree"]
async fn helper_case() {
    let file = PathBuf::from(std::env::var_os(HELPER).expect("run by its test"));
    let cwd = file.parent().expect("dir").to_path_buf();
    // Never returns: the test terminates this process.
    let _ = exec_command(
        "powershell".as_ref(),
        &powershell(
            Some(&file.with_file_name("leader")),
            &file,
            "Start-Sleep -Seconds 600",
        ),
        &cwd,
        ExecOptions::default(),
    )
    .await;
}

/// How a child `nested_case` tried to start began: running, held here by
/// handle, or refused with an OS error code.
enum Start {
    Spawned(Background),
    Failed(i32),
}

/// Waits, at most [`BOUND`], for `nested_case`'s report in `dir`.
async fn nested_report(dir: &Path) -> Result<String, String> {
    let file = dir.join("report");
    let deadline = Instant::now() + BOUND;
    loop {
        if let Ok(report) = std::fs::read_to_string(&file) {
            return Ok(report);
        }
        if Instant::now() >= deadline {
            return Err("nested_case never reported".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The start `report` gives for `name`: `spawned <pid>`, whose process is
/// opened here at once, or `failed <os error>`.
fn start(report: &str, name: &str) -> Result<Start, String> {
    let outcome = report
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
        .ok_or_else(|| format!("{name}: not in the report {report:?}"))?;
    match outcome.split_once(' ') {
        Some(("spawned", pid)) => pid
            .parse()
            .map_err(|_| format!("{name}: a malformed pid {pid:?}"))
            .and_then(Background::open_pid)
            .map(Start::Spawned),
        Some(("failed", code)) => code
            .parse()
            .map(Start::Failed)
            .map_err(|_| format!("{name}: a malformed error {code:?}")),
        _ => Err(format!("{name}: a malformed outcome {outcome:?}")),
    }
}

// A descendant's own job is no way out of the call's tree. The command
// starts `nested_case`, a member of the call's job. It asks for a child
// with `CREATE_BREAKAWAY_FROM_JOB`, which the call's job, without
// `JOB_OBJECT_LIMIT_BREAKAWAY_OK`, refuses. It then joins a job of its own
// that allows breakaway, which Windows nests in the call's job, and starts
// a child there and one that breaks away from it; the second stops at the
// call's job, which does not allow breakaway (Microsoft's "Nested Jobs").
// An abort ends both children and returns only once they have ended: a
// nested job's new processes are posted to the call's job's port and
// counted in its accounting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_ends_a_nested_job_and_nothing_breaks_away() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let abort = AbortController::new();
    let owner = WorkGate::default();
    let command = format!(
        "$env:{NESTED} = {}; & {} --exact nested_case --ignored --nocapture",
        quoted(&dir.0),
        quoted(&std::env::current_exe().expect("exe"))
    );
    let mut call = exec(
        vec!["-NoProfile".into(), "-Command".into(), command.into()],
        dir.0.clone(),
        ExecOptions {
            signal: Some(abort.signal()),
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let report = nested_report(&dir.0).await;
    let [direct, nested, breakaway] = ["direct", "nested", "breakaway"].map(|name| {
        report
            .as_deref()
            .map_err(String::clone)
            .and_then(|report| start(report, name))
    });
    abort.abort();
    let mut observed = Vec::new();
    match call.join(BOUND).await {
        Ok(result) => observed.push(check(
            result.aborted && result.killed,
            &format!("not an aborted kill: {result:?}"),
        )),
        Err(error) => observed.push(Err(error)),
    }
    let denied = ERROR_ACCESS_DENIED as i32;
    observed.push(match &direct {
        Ok(Start::Failed(code)) => check(
            *code == denied,
            &format!("the breakaway failed otherwise: os error {code}"),
        ),
        Ok(Start::Spawned(_)) => Err("a child broke away from the call's job".into()),
        Err(error) => Err(error.clone()),
    });
    // The call returned only once its job was empty: already ended.
    observed.push(match &nested {
        Ok(Start::Spawned(held)) => check(
            held.ended_within(Duration::ZERO).await,
            "the call returned while a nested job's process ran",
        ),
        Ok(Start::Failed(code)) => Err(format!(
            "the nested job's child did not start: os error {code}"
        )),
        Err(error) => Err(error.clone()),
    });
    observed.push(match &breakaway {
        Ok(Start::Spawned(held)) => check(
            held.ended_within(Duration::ZERO).await,
            "a child that broke away from the nested job outlived the call's job",
        ),
        // Refused outright, it is not out of the tree either.
        Ok(Start::Failed(code)) => check(
            *code == denied,
            &format!("the nested breakaway failed otherwise: os error {code}"),
        ),
        Err(error) => Err(error.clone()),
    });
    let stopped = call.stop().await;
    let first_close = close(&owner).await;
    let mut cleanup = vec![stopped, first_close.clone()];
    for start in [direct, nested, breakaway] {
        if let Ok(Start::Spawned(held)) = start {
            cleanup.push(held.stop().await);
        }
    }
    redrain(&owner, &first_close, &mut cleanup).await;
    verdict(observed, cleanup);
}

/// A new job that allows breakaway, with this process in it: nested in the
/// job this process is already in.
fn nested_job() -> std::io::Result<OwnedHandle> {
    // SAFETY: null attributes (not inheritable) and no name.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a fresh handle owned here.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: a zeroed struct is a valid starting value.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| std::io::Error::other("size"))?;
    // SAFETY: `limits` has the size passed; the job handle is live.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            size,
        )
    };
    if set == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a live job handle and this process's pseudo handle.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(job)
}

// Started, in the call's job, by the command of
// `an_abort_ends_a_nested_job_and_nothing_breaks_away`. Reports each
// start, `<name> spawned <pid>` or `<name> failed <os error>`, in one
// `report` written all at once, then waits to be ended with the call's
// job. Should the case never end it, a watchdog ends its children, then
// fails.
#[test]
#[ignore = "started by the command of an_abort_ends_a_nested_job_and_nothing_breaks_away"]
fn nested_case() {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    let dir = PathBuf::from(std::env::var_os(NESTED).expect("started by its case's command"));
    let spawn = |flags: u32| {
        Command::new("ping")
            .args(["-n", "600", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
            .map_err(|error| error.raw_os_error().unwrap_or(-1))
    };
    let direct = spawn(CREATE_BREAKAWAY_FROM_JOB);
    let job = nested_job();
    let (nested, breakaway) = match &job {
        Ok(_) => (spawn(0), spawn(CREATE_BREAKAWAY_FROM_JOB)),
        Err(error) => {
            eprintln!("the nested job: {error}");
            let code = error.raw_os_error().unwrap_or(-1);
            (Err(code), Err(code))
        }
    };
    let mut lines = Vec::new();
    let mut children = Vec::new();
    for (name, started) in [
        ("direct", direct),
        ("nested", nested),
        ("breakaway", breakaway),
    ] {
        match started {
            Ok(child) => {
                lines.push(format!("{name} spawned {}", child.id()));
                children.push(child);
            }
            Err(code) => lines.push(format!("{name} failed {code}")),
        }
    }
    let partial = dir.join("report.partial");
    let published = std::fs::write(&partial, lines.join("\n"))
        .and_then(|()| std::fs::rename(&partial, dir.join("report")));
    if published.is_ok() {
        // The abort ends this process, a member of the call's job, first.
        std::thread::sleep(Duration::from_secs(600));
    }
    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("never ended by its case: {published:?}");
}
