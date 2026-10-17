//! Windows job settlement: a native observation and production-path cases.
//! `harness = false`: the binary re-executes itself as its own helper
//! processes, so their standard output is the protocol.
//!
//! 1. **Observation, not qualification.** A private, named kill-on-close job
//!    holds a parent that starts a long-lived child inheriting the parent's
//!    piped standard output, then exits. The job is terminated, and each run
//!    prints how the job's active count, its process-id list, a fresh
//!    `OpenProcess` of the child's pid with `IsProcessInJob`, the child's
//!    retained handle and exit code, and the end of its inherited pipe change
//!    until that handle is signalled. The summary counts the runs in which
//!    the count reached 0 while the handle was not yet signalled. Zero such
//!    runs does not show that the window cannot occur, and an unsignalled
//!    handle shows only that the end is not yet observed, not that user code
//!    runs. Observations fail nothing.
//! 2. **Production path.** `bake_process::OwnedProcess` spawns a parent in
//!    its own job; the parent starts a child, and the test retains its own
//!    handle to that child while the parent still holds it, so the pid names
//!    it. The parent exits by itself; the owner is killed and settled, as an
//!    MCP shutdown does. Once `finish` reports `TreeState::Gone`, the
//!    retained handle must already be signalled, checked once with no wait.
//!    A failure here is a real failure of the owner's settlement.
//! 3. **Production path under churn.** The parent keeps starting children
//!    that exit at once and long-lived children whose pids it reports and
//!    whose handles it keeps; the test retains 16 of the long-lived ones
//!    and kills the tree while the parent is still starting more. After
//!    `Gone`, every retained handle must already be signalled. A settle
//!    that does not report within the bound is counted apart, as
//!    incomplete: it reported nothing false, but did not show the tree
//!    settled.
//! 4. **Production path, churn while the owner is idle.** The same parent
//!    starts all its children while the owner only reads its output, as a
//!    long-lived MCP server's owner does between requests; the live job
//!    pump is the only thing taking the job's messages then. The tree is
//!    then killed, and the same immediate check applies.
//!
//! Containment: a parent spawns its child only after it has read the exact
//! line `go` and confirmed, through the job's name, that it is in the job;
//! end of input or anything else makes it exit with nothing spawned. Every
//! run owns its job, parent, confirmed child handle, and pipe reader in one
//! guard whose cleanup terminates the job, waits (bounded) for each process
//! through a handle it owns, and joins the reader once its pipe has ended. A
//! pid that was not confirmed is never signalled. A cleanup that does not
//! observe its processes and pipe end is a failure.
//!
//! Runs: 50 observations, 20 production-path cases, 10 churn cases, and 5
//! idle churn cases by default; `BAKE_PROCESS_JOB_PROBE_RUNS`,
//! `BAKE_PROCESS_SETTLEMENT_RUNS`, `BAKE_PROCESS_CHURN_RUNS`, and
//! `BAKE_PROCESS_IDLE_CHURN_RUNS` change them, capped at 500, 200, 100, and
//! 50.
//! Exit status 1 means a fixture or cleanup failure, a production-path
//! failure, or an incomplete settle; each is reported apart.

#[cfg(not(windows))]
fn main() {
    println!("SKIP: the Windows job settlement probe and case run on Windows only");
}

#[cfg(windows)]
fn main() {
    windows::main();
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use bake_process::{OwnedProcess, ProcessSpec, TreeState};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use windows_sys::Win32::Foundation::{
        ERROR_MORE_DATA, GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    const MODE: &str = "BAKE_PROCESS_JOB_PROBE_MODE";
    const JOB_NAME: &str = "BAKE_PROCESS_JOB_PROBE_JOB";
    /// `JOB_OBJECT_QUERY` from `winnt.h`.
    const JOB_OBJECT_QUERY: u32 = 0x0004;
    /// Every wait, and the sampling after a termination.
    const BOUND: Duration = Duration::from_secs(10);
    /// The longest helper line read.
    const LINE_LIMIT: u64 = 64;
    /// The most transitions kept per run.
    const TRANSITIONS: usize = 32;

    pub fn main() {
        match std::env::var(MODE).as_deref() {
            Ok("parent") => std::process::exit(parent()),
            Ok("settle-parent") => std::process::exit(settle_parent()),
            Ok("churn-parent") => std::process::exit(churn_parent()),
            Ok("exit") => std::process::exit(0),
            Ok("child") => loop {
                std::thread::sleep(Duration::from_secs(3600));
            },
            _ => {}
        }
        let probe_runs = runs("BAKE_PROCESS_JOB_PROBE_RUNS", 50, 500);
        let settle_runs = runs("BAKE_PROCESS_SETTLEMENT_RUNS", 20, 200);
        let churn_runs = runs("BAKE_PROCESS_CHURN_RUNS", 10, 100);
        let idle_runs = runs("BAKE_PROCESS_IDLE_CHURN_RUNS", 5, 50);
        let version = Command::new("cmd")
            .args(["/c", "ver"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default();
        println!("windows: {version}");

        println!("\nobservation: {probe_runs} runs");
        let mut summary = Summary::default();
        for run in 0..probe_runs {
            let mut guard = ProbeRun::default();
            let observed = probe(run, &mut guard);
            let cleanup = guard.clean_up();
            match (observed, cleanup.is_empty()) {
                (Ok(observed), true) => summary.add(&observed),
                (observed, _) => {
                    summary.fixture_failures += 1;
                    if let Err(error) = observed {
                        println!("run {run}: FIXTURE FAILURE: {error}");
                    }
                    for line in cleanup {
                        println!("run {run}: CLEANUP FAILURE: {line}");
                    }
                }
            }
        }
        summary.print(probe_runs);

        println!("\nproduction path: {settle_runs} cases");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let mut single = Tally::default();
        for case in 0..settle_runs {
            let outcome = runtime.block_on(settle_case());
            single.add(&outcome.verdict);
            println!("case {case}: {outcome}");
        }
        single.print("production path", settle_runs);

        println!("\nproduction path under churn: {churn_runs} cases");
        let mut churn = Tally::default();
        for case in 0..churn_runs {
            let outcome = runtime.block_on(churn_case(false));
            churn.add(&outcome.verdict);
            println!("churn {case}: {outcome}");
        }
        churn.print("production path under churn", churn_runs);

        println!("\nproduction path, churn while the owner is idle: {idle_runs} cases");
        let mut idle = Tally::default();
        for case in 0..idle_runs {
            let outcome = runtime.block_on(churn_case(true));
            idle.add(&outcome.verdict);
            println!("idle churn {case}: {outcome}");
        }
        drop(runtime);
        idle.print("production path, churn while the owner is idle", idle_runs);

        if summary.fixture_failures > 0 || !single.clean() || !churn.clean() || !idle.clean() {
            println!("\nresult: FAILED");
            std::process::exit(1);
        }
        println!("\nresult: ok");
    }

    fn runs(variable: &str, default: usize, cap: usize) -> usize {
        match std::env::var(variable)
            .ok()
            .map(|value| value.parse::<usize>())
        {
            None => default,
            Some(Ok(runs)) if (1..=cap).contains(&runs) => runs,
            Some(_) => {
                println!("{variable} must be 1..={cap}; using {default}");
                default
            }
        }
    }

    // ------------------------------------------------------------ helpers

    /// Reads the exact line `expected` from standard input, bounded in size.
    fn read_exact_line(expected: &str) -> bool {
        let mut line = String::new();
        let stdin = std::io::stdin();
        let read = stdin.lock().take(LINE_LIMIT).read_line(&mut line);
        matches!(read, Ok(n) if n > 0) && line.trim_end() == expected
    }

    /// The observation's parent: spawns its child only after `go` and only
    /// once it is in the named job.
    fn parent() -> i32 {
        if !read_exact_line("go") {
            return 2;
        }
        let Ok(name) = std::env::var(JOB_NAME) else {
            return 3;
        };
        let wide: Vec<u16> = OsStr::new(&name).encode_wide().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated name; a null handle is an error.
        let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, wide.as_ptr()) };
        if raw.is_null() {
            return 3;
        }
        // SAFETY: a fresh handle owned only here.
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut member = 0;
        // SAFETY: the pseudo handle and the job handle are live.
        let ok = unsafe { IsProcessInJob(GetCurrentProcess(), job.as_raw_handle(), &mut member) };
        if ok == 0 || member == 0 {
            return 3;
        }
        let Ok(child) = Command::new(std::env::current_exe().expect("own path"))
            .env(MODE, "child")
            .env_remove(JOB_NAME)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return 4;
        };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "child {}", child.id());
        let _ = out.flush();
        // Dropping `child` closes this process's handle; the child stays in
        // the job, which the probe terminates.
        0
    }

    /// The production case's parent, born in the owner's job: starts its
    /// child, reports it, and keeps the child's handle until the test says
    /// it holds its own.
    fn settle_parent() -> i32 {
        let Ok(child) = Command::new(std::env::current_exe().expect("own path"))
            .env(MODE, "child")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return 4;
        };
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "child {}", child.id());
            let _ = out.flush();
        }
        // Any input or its end lets the parent exit: the child is in the
        // owner's job either way.
        let _ = read_exact_line("opened");
        drop(child);
        0
    }

    fn last_error() -> std::io::Error {
        std::io::Error::last_os_error()
    }

    fn open(pid: u32) -> Option<OwnedHandle> {
        // SAFETY: plain call; a null handle is an error.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        // SAFETY: a fresh handle owned only here.
        (!raw.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    fn signalled(handle: HANDLE) -> Option<bool> {
        // SAFETY: a live handle; a zero timeout only checks.
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 => Some(true),
            WAIT_TIMEOUT => Some(false),
            _ => None,
        }
    }

    /// Waits, bounded, for `handle`; true once it is signalled.
    fn wait_bounded(handle: HANDLE) -> bool {
        // SAFETY: a live handle; bounded wait.
        unsafe { WaitForSingleObject(handle, BOUND.as_millis() as u32) == WAIT_OBJECT_0 }
    }

    fn exit_code(handle: HANDLE) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: a live handle and a valid out pointer.
        (unsafe { GetExitCodeProcess(handle, &mut code) } != 0).then_some(code)
    }

    // ------------------------------------------------------------ the job

    struct Job {
        handle: OwnedHandle,
        name: String,
    }

    impl Job {
        fn new() -> Result<Self, String> {
            let mut random = [0u8; 8];
            getrandom::getrandom(&mut random).map_err(|_| "no random source".to_owned())?;
            let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let name = format!(r"Local\bake-process-job-probe-{}-{id}", std::process::id());
            let wide: Vec<u16> = OsStr::new(&name).encode_wide().chain(Some(0)).collect();
            // SAFETY: null attributes and a NUL-terminated name.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), wide.as_ptr()) };
            if raw.is_null() {
                return Err(format!("CreateJobObjectW: {}", last_error()));
            }
            // SAFETY: reading the thread's last error has no preconditions.
            let existed =
                unsafe { GetLastError() } == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
            // SAFETY: a fresh handle owned only here.
            let job = Self {
                handle: unsafe { OwnedHandle::from_raw_handle(raw) },
                name,
            };
            if existed {
                return Err("the probe's job name already existed".into());
            }
            // SAFETY: a zeroed struct is a valid starting value.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` has the size passed.
            if unsafe {
                SetInformationJobObject(
                    job.raw(),
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(format!("SetInformationJobObject: {}", last_error()));
            }
            Ok(job)
        }

        fn raw(&self) -> HANDLE {
            self.handle.as_raw_handle()
        }

        /// `(ActiveProcesses, TotalProcesses, TotalTerminatedProcesses)`.
        fn counts(&self) -> Option<(u32, u32, u32)> {
            // SAFETY: a zeroed struct is valid output space.
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` has the size passed.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.raw(),
                    JobObjectBasicAccountingInformation,
                    std::ptr::addr_of_mut!(info).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            };
            (ok != 0).then_some((
                info.ActiveProcesses,
                info.TotalProcesses,
                info.TotalTerminatedProcesses,
            ))
        }

        /// The listed pids (at most 64) and `NumberOfAssignedProcesses`.
        fn pids(&self) -> Option<(Vec<usize>, usize)> {
            const CAPACITY: usize = 64;
            let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
            let mut buffer = vec![0usize; header / size_of::<usize>() + CAPACITY];
            let length = (buffer.len() * size_of::<usize>()) as u32;
            // SAFETY: `buffer` holds `length` bytes, aligned for the struct.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.raw(),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    length,
                    std::ptr::null_mut(),
                )
            };
            // SAFETY: reading the thread's last error has no preconditions.
            if ok == 0 && unsafe { GetLastError() } != ERROR_MORE_DATA {
                return None;
            }
            // SAFETY: the header was written.
            let (assigned, listed) = unsafe {
                let list = &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
                (
                    list.NumberOfAssignedProcesses as usize,
                    list.NumberOfProcessIdsInList as usize,
                )
            };
            Some((
                buffer[header / size_of::<usize>()..][..listed.min(CAPACITY)].to_vec(),
                assigned,
            ))
        }

        fn holds(&self, process: HANDLE) -> Option<bool> {
            let mut member = 0;
            // SAFETY: both handles are live.
            let ok = unsafe { IsProcessInJob(process, self.raw(), &mut member) };
            (ok != 0).then_some(member != 0)
        }

        fn terminate(&self) -> bool {
            // SAFETY: a live job handle.
            unsafe { TerminateJobObject(self.raw(), 1) != 0 }
        }
    }

    // ------------------------------------------------------------ the pipe

    /// A thread that reads one bounded line, then drains the pipe to its
    /// end, discarding (and counting) what follows.
    struct Reader {
        thread: std::thread::JoinHandle<()>,
        line: mpsc::Receiver<Result<String, String>>,
        ended: mpsc::Receiver<(Instant, u64)>,
    }

    impl Reader {
        fn start(pipe: impl Read + Send + 'static) -> Self {
            let (send_line, line) = mpsc::channel();
            let (send_end, ended) = mpsc::channel();
            let thread = std::thread::spawn(move || {
                let mut reader = BufReader::new(pipe);
                let mut first = String::new();
                let read = (&mut reader)
                    .take(LINE_LIMIT)
                    .read_line(&mut first)
                    .map(|_| first)
                    .map_err(|error| error.to_string());
                let _ = send_line.send(read);
                let mut rest = 0u64;
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => rest += read as u64,
                    }
                }
                let _ = send_end.send((Instant::now(), rest));
            });
            Self {
                thread,
                line,
                ended,
            }
        }
    }

    // ------------------------------------------------------------ observation

    /// What one observation run owns, cleaned up in one place.
    #[derive(Default)]
    struct ProbeRun {
        job: Option<Job>,
        parent: Option<std::process::Child>,
        child: Option<OwnedHandle>,
        /// A pid the parent reported that was never confirmed in the job.
        unconfirmed: Option<u32>,
        reader: Option<Reader>,
        /// When the pipe ended, if the observation already saw it.
        pipe_ended: Option<(Instant, u64)>,
    }

    impl ProbeRun {
        /// Terminates the job and waits, bounded, for every process through
        /// a handle owned here and for the pipe to end, then joins the
        /// reader. Returns what was not observed to end.
        fn clean_up(&mut self) -> Vec<String> {
            let mut left = Vec::new();
            if let Some(job) = &self.job
                && !job.terminate()
            {
                left.push(format!("TerminateJobObject: {}", last_error()));
            }
            if let Some(mut parent) = self.parent.take() {
                if !wait_bounded(parent.as_raw_handle()) {
                    // Not in the job (the assignment failed) or not ending:
                    // ours by handle, so ending it reaches only it.
                    let _ = parent.kill();
                }
                if wait_bounded(parent.as_raw_handle()) {
                    let _ = parent.wait();
                } else {
                    left.push(format!("the parent ({}) did not exit", parent.id()));
                }
            }
            if let Some(child) = self.child.take()
                && !wait_bounded(child.as_raw_handle())
            {
                // SAFETY: a live handle to the confirmed child.
                unsafe {
                    TerminateProcess(child.as_raw_handle(), 1);
                }
                if !wait_bounded(child.as_raw_handle()) {
                    left.push("the confirmed child did not exit".into());
                }
            }
            if let Some(pid) = self.unconfirmed.take() {
                left.push(format!(
                    "child pid {pid} was never confirmed in the job; not signalled"
                ));
            }
            if let Some(reader) = self.reader.take() {
                let ended = self
                    .pipe_ended
                    .take()
                    .or_else(|| reader.ended.recv_timeout(BOUND).ok());
                if ended.is_some() {
                    let _ = reader.thread.join();
                } else {
                    // Still blocked on a pipe something holds open: it cannot
                    // be joined, and is reported.
                    left.push("the output pipe did not end; its reader is left blocked".into());
                }
            }
            self.job = None;
            left
        }
    }

    impl Drop for ProbeRun {
        fn drop(&mut self) {
            // A last resort if the caller did not clean up.
            for line in self.clean_up() {
                println!("cleanup on drop: {line}");
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Sample {
        active: Option<u32>,
        terminated: Option<u32>,
        listed: Option<bool>,
        listed_count: Option<usize>,
        assigned: Option<usize>,
        /// A fresh `OpenProcess(pid)`: `in_job`, `not_in_job`,
        /// `in_job_unknown`, or `open_error=<code>`.
        reopened: String,
        signalled: Option<bool>,
        exit_code: Option<u32>,
    }

    fn sample(job: &Job, pid: u32, child: &OwnedHandle) -> Sample {
        let counts = job.counts();
        let pids = job.pids();
        let reopened = match open(pid) {
            Some(handle) => match job.holds(handle.as_raw_handle()) {
                Some(true) => "in_job".to_owned(),
                Some(false) => "not_in_job".to_owned(),
                None => "in_job_unknown".to_owned(),
            },
            None => format!("open_error={}", last_error().raw_os_error().unwrap_or(-1)),
        };
        Sample {
            active: counts.map(|counts| counts.0),
            terminated: counts.map(|counts| counts.2),
            listed: pids
                .as_ref()
                .map(|(pids, _)| pids.contains(&(pid as usize))),
            listed_count: pids.as_ref().map(|(pids, _)| pids.len()),
            assigned: pids.as_ref().map(|(_, assigned)| *assigned),
            reopened,
            signalled: signalled(child.as_raw_handle()),
            exit_code: exit_code(child.as_raw_handle()),
        }
    }

    struct Observed {
        window: bool,
        window_always_listed: bool,
        window_always_reopened: bool,
        window_micros: u128,
        signalled_micros: u128,
    }

    fn probe(run: usize, guard: &mut ProbeRun) -> Result<Observed, String> {
        let job = guard.job.insert(Job::new()?);
        let parent = guard.parent.insert(
            Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .env(MODE, "parent")
                .env(JOB_NAME, &job.name)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| format!("spawn the parent: {error}"))?,
        );
        let stdout = parent.stdout.take().ok_or("no parent stdout")?;
        let mut stdin = parent.stdin.take().ok_or("no parent stdin")?;
        let reader = guard.reader.insert(Reader::start(stdout));
        // Until `go`, the parent only waits; on any return before it, the
        // dropped stdin ends its input and it exits with nothing spawned.
        // SAFETY: both handles are live.
        if unsafe { AssignProcessToJobObject(job.raw(), parent.as_raw_handle()) } == 0 {
            return Err(format!("AssignProcessToJobObject: {}", last_error()));
        }
        if job.holds(parent.as_raw_handle()) != Some(true) {
            return Err("the parent is not in the probe's job".into());
        }
        stdin
            .write_all(b"go\n")
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("release the parent: {error}"))?;
        drop(stdin);
        let line = match reader.line.recv_timeout(BOUND) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => return Err(format!("reading the parent: {error}")),
            Err(_) => return Err("no line from the parent".into()),
        };
        let pid: u32 = line
            .trim()
            .strip_prefix("child ")
            .and_then(|pid| pid.parse().ok())
            .ok_or_else(|| format!("unexpected parent output {line:?}"))?;
        guard.unconfirmed = Some(pid);
        if !wait_bounded(parent.as_raw_handle()) {
            return Err("the parent did not exit".into());
        }
        // The parent has exited; the pid is confirmed by membership of this
        // private job, which only this run's processes join.
        let child = open(pid).ok_or_else(|| format!("OpenProcess({pid}): {}", last_error()))?;
        if job.holds(child.as_raw_handle()) != Some(true) {
            return Err(format!("pid {pid} is not in the probe's job"));
        }
        guard.unconfirmed = None;
        let child = guard.child.insert(child);

        let before = sample(job, pid, child);
        let started = Instant::now();
        let terminated = job.terminate();
        let mut transitions = vec![(0u128, before.clone())];
        let mut dropped = 0usize;
        let mut last = before;
        let mut window_first: Option<Instant> = None;
        let mut window_always_listed = true;
        let mut window_always_reopened = true;
        let mut signalled_micros = None;
        while started.elapsed() < BOUND {
            let now = sample(job, pid, child);
            let at = started.elapsed().as_micros();
            if now.active == Some(0) && now.signalled == Some(false) {
                window_first.get_or_insert_with(Instant::now);
                window_always_listed &= now.listed == Some(true);
                window_always_reopened &= now.reopened == "in_job";
            }
            if now != last {
                if transitions.len() < TRANSITIONS {
                    transitions.push((at, now.clone()));
                } else {
                    dropped += 1;
                }
                last = now.clone();
            }
            if now.signalled == Some(true) {
                signalled_micros = Some(at);
                break;
            }
            std::thread::yield_now();
        }
        let after = (started.elapsed().as_micros(), sample(job, pid, child));
        let window_micros = window_first.map_or(0, |first| first.elapsed().as_micros());
        let pipe = reader.ended.recv_timeout(BOUND).ok();
        guard.pipe_ended = pipe;
        let text: Vec<String> = transitions
            .iter()
            .chain(std::iter::once(&after))
            .map(|(at, s)| {
                format!(
                    "+{at}us active={:?} terminated={:?} listed={:?}/{:?} assigned={:?} reopened={} signalled={:?} exit_code={:?}",
                    s.active, s.terminated, s.listed, s.listed_count, s.assigned, s.reopened, s.signalled, s.exit_code
                )
            })
            .collect();
        let pipe_text = match pipe {
            Some((at, bytes)) => format!(
                "+{}us ({bytes} bytes after the first line)",
                at.saturating_duration_since(started).as_micros()
            ),
            None => "not within the bound".to_owned(),
        };
        println!(
            "run {run}: TerminateJobObject={terminated} window={} pipe_ended={pipe_text}{} | {}",
            window_first.is_some(),
            if dropped > 0 {
                format!(" ({dropped} transitions not shown)")
            } else {
                String::new()
            },
            text.join(" | ")
        );
        let Some(signalled_micros) = signalled_micros else {
            return Err(format!("the child was not signalled within {BOUND:?}"));
        };
        if pipe.is_none() {
            return Err("the child's inherited output did not end".into());
        }
        Ok(Observed {
            window: window_first.is_some(),
            window_always_listed,
            window_always_reopened,
            window_micros,
            signalled_micros,
        })
    }

    #[derive(Default)]
    struct Summary {
        runs: usize,
        fixture_failures: usize,
        windows: usize,
        always_listed: usize,
        always_reopened: usize,
        longest_window_micros: u128,
        longest_signal_micros: u128,
    }

    impl Summary {
        fn add(&mut self, observed: &Observed) {
            self.runs += 1;
            self.longest_signal_micros = self.longest_signal_micros.max(observed.signalled_micros);
            if observed.window {
                self.windows += 1;
                self.always_listed += usize::from(observed.window_always_listed);
                self.always_reopened += usize::from(observed.window_always_reopened);
                self.longest_window_micros = self.longest_window_micros.max(observed.window_micros);
            }
        }

        fn print(&self, requested: usize) {
            println!(
                "observation: requested={requested} observed={} fixture_or_cleanup_failures={} \
                 count_zero_while_unsignalled_runs={} of_which_always_listed={} \
                 of_which_always_reopenable_in_job={} longest_window_us={} \
                 longest_terminate_to_signal_us={} (zero runs does not show the window cannot occur)",
                self.runs,
                self.fixture_failures,
                self.windows,
                self.always_listed,
                self.always_reopened,
                self.longest_window_micros,
                self.longest_signal_micros
            );
        }
    }

    // ------------------------------------------------------------ production path

    #[derive(Default)]
    struct Tally {
        passed: usize,
        failed: usize,
        incomplete: usize,
        fixture: usize,
    }

    impl Tally {
        fn add(&mut self, verdict: &Verdict) {
            match verdict {
                Verdict::Passed => self.passed += 1,
                Verdict::Failed(_) => self.failed += 1,
                Verdict::Incomplete(_) => self.incomplete += 1,
                Verdict::Fixture(_) => self.fixture += 1,
            }
        }

        fn clean(&self) -> bool {
            self.failed == 0 && self.incomplete == 0 && self.fixture == 0
        }

        fn print(&self, what: &str, cases: usize) {
            println!(
                "{what}: cases={cases} passed={} \
                 retained_descendant_unsignalled_after_gone={} \
                 settlement_incomplete_within_bound={} fixture_or_cleanup_failures={}",
                self.passed, self.failed, self.incomplete, self.fixture
            );
        }
    }

    enum Verdict {
        Passed,
        /// `finish` reported `Gone` while the retained descendant's handle
        /// was not signalled.
        Failed(String),
        /// `finish` did not report within the bound: not unsafe (nothing
        /// was reported gone), but the tree was not shown settled.
        Incomplete(String),
        Fixture(String),
    }

    struct Outcome {
        verdict: Verdict,
        notes: Vec<String>,
    }

    impl std::fmt::Display for Outcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match &self.verdict {
                Verdict::Passed => f.write_str("ok")?,
                Verdict::Failed(why) => write!(f, "FAILED: {why}")?,
                Verdict::Incomplete(why) => write!(f, "INCOMPLETE: {why}")?,
                Verdict::Fixture(why) => write!(f, "FIXTURE FAILURE: {why}")?,
            }
            for note in &self.notes {
                write!(f, "; {note}")?;
            }
            Ok(())
        }
    }

    async fn settle_case() -> Outcome {
        let mut notes = Vec::new();
        let program = match std::env::current_exe() {
            Ok(program) => program,
            Err(error) => {
                return Outcome {
                    verdict: Verdict::Fixture(error.to_string()),
                    notes,
                };
            }
        };
        let mut env: Vec<_> = std::env::vars_os().collect();
        env.push((MODE.into(), "settle-parent".into()));
        let spec = ProcessSpec {
            program: program.into_os_string(),
            args: Vec::new(),
            cwd: None,
            env: Some(env),
            stdin: true,
        };
        let mut process = match OwnedProcess::spawn(&spec, None).await {
            Ok(process) => process,
            Err(error) => {
                return Outcome {
                    verdict: Verdict::Fixture(format!("spawn: {error}")),
                    notes,
                };
            }
        };
        let mut child: Option<OwnedHandle> = None;
        let mut settled = false;
        let mut stdout = process.take_stdout();
        let verdict = settle_body(&mut process, &mut child, &mut settled, &mut stdout).await;
        // Cleanup, on every path: the owner's tree killed and settled, the
        // retained child awaited through its handle, the pipe ended.
        if !settled {
            process.kill();
            match tokio::time::timeout(BOUND, process.finish()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => notes.push(format!("cleanup settle failed: {error}")),
                Err(_) => notes.push("cleanup settle did not finish".into()),
            }
        }
        if let Some(child) = child.take()
            && !wait_bounded(child.as_raw_handle())
        {
            // SAFETY: a live handle to the retained child.
            unsafe {
                TerminateProcess(child.as_raw_handle(), 1);
            }
            if !wait_bounded(child.as_raw_handle()) {
                notes.push("cleanup: the retained child did not exit".into());
            }
        }
        if let Some(mut stdout) = stdout.take() {
            let mut sink = Vec::new();
            match tokio::time::timeout(BOUND, (&mut stdout).take(1 << 16).read_to_end(&mut sink))
                .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => notes.push(format!("cleanup: output pipe error {error}")),
                Err(_) => notes.push("cleanup: the output pipe did not end".into()),
            }
        }
        let cleanup_failed = notes.iter().any(|note| note.starts_with("cleanup"));
        let verdict = match verdict {
            Verdict::Passed if cleanup_failed => Verdict::Fixture("cleanup".into()),
            verdict => verdict,
        };
        Outcome { verdict, notes }
    }

    async fn settle_body(
        process: &mut OwnedProcess,
        child: &mut Option<OwnedHandle>,
        settled: &mut bool,
        stdout: &mut Option<bake_process::ProcessStdout>,
    ) -> Verdict {
        let Some(pipe) = stdout.as_mut() else {
            return Verdict::Fixture("no stdout".into());
        };
        let mut line = String::new();
        let mut reader = tokio::io::BufReader::new(pipe).take(LINE_LIMIT);
        match tokio::time::timeout(BOUND, reader.read_line(&mut line)).await {
            Ok(Ok(n)) if n > 0 => {}
            _ => return Verdict::Fixture("no child line from the parent".into()),
        }
        let Some(pid) = line
            .trim()
            .strip_prefix("child ")
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            return Verdict::Fixture(format!("unexpected parent output {line:?}"));
        };
        // The parent holds its child's handle until it reads `opened`, so
        // while it runs the pid names that child.
        let Some(handle) = open(pid) else {
            return Verdict::Fixture(format!("OpenProcess({pid}): {}", last_error()));
        };
        let parent_running = tokio::select! {
            biased;
            _ = process.wait_exit() => false,
            () = std::future::ready(()) => true,
        };
        if !parent_running {
            // Unverified: the handle is dropped, never signalled.
            return Verdict::Fixture("the parent exited before the child was confirmed".into());
        }
        let handle = child.insert(handle);
        let Some(mut stdin) = process.take_stdin() else {
            return Verdict::Fixture("no stdin".into());
        };
        let _ = stdin.write_all(b"opened\n").await;
        let _ = stdin.shutdown().await;
        drop(stdin);
        match tokio::time::timeout(BOUND, process.wait_exit()).await {
            Ok(Ok(())) => {}
            _ => return Verdict::Fixture("the parent did not exit".into()),
        }
        // As an MCP shutdown does after a server exits by itself.
        process.kill();
        let report = match tokio::time::timeout(BOUND, process.finish()).await {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => return Verdict::Fixture(format!("finish: {error}")),
            Err(_) => return Verdict::Incomplete(format!("finish did not complete in {BOUND:?}")),
        };
        *settled = true;
        if report.tree != TreeState::Gone {
            return Verdict::Failed(format!("the killed tree settled as {:?}", report.tree));
        }
        // Checked once, with no wait: a later exit would not prove it.
        match signalled(handle.as_raw_handle()) {
            Some(true) => Verdict::Passed,
            Some(false) => {
                let code = exit_code(handle.as_raw_handle());
                let started = Instant::now();
                let later = wait_bounded(handle.as_raw_handle());
                Verdict::Failed(format!(
                    "finish reported Gone with the retained descendant ({pid}) unsignalled \
                     (exit code then {code:?}; {} {}us later)",
                    if later {
                        "signalled"
                    } else {
                        "still unsignalled"
                    },
                    started.elapsed().as_micros()
                ))
            }
            None => Verdict::Fixture("the retained handle could not be checked".into()),
        }
    }

    // ------------------------------------------------------------ churn

    /// Children the churn parent starts at most, of each kind.
    const CHURN: usize = 200;
    /// Long-lived children the test retains before it kills the tree.
    const RETAINED: usize = 16;

    /// The churn case's parent, born in the owner's job: starts, over and
    /// over, a child that exits at once (its handle closed, so it is
    /// already exiting or gone when the tree is killed) and a long-lived
    /// child whose pid it reports and whose handle it keeps, so a reported
    /// pid names that child while the parent runs. It is still starting
    /// children when it is killed. After [`CHURN`] of each it waits for
    /// input, keeping its handles.
    fn churn_parent() -> i32 {
        let program = std::env::current_exe().expect("own path");
        let mut held = Vec::new();
        for _ in 0..CHURN {
            let _ = Command::new(&program)
                .env(MODE, "exit")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            let Ok(child) = Command::new(&program)
                .env(MODE, "child")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                return 4;
            };
            let mut out = std::io::stdout().lock();
            if writeln!(out, "child {}", child.id())
                .and_then(|()| out.flush())
                .is_err()
            {
                return 5;
            }
            held.push(child);
        }
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "done").and_then(|()| out.flush());
        }
        let _ = read_exact_line("never");
        drop(held);
        0
    }

    /// `idle`: the owner only reads the parent's output until the parent
    /// has started all its children, so the tree's whole churn happens
    /// while the owner neither waits for nor settles it; then it is killed.
    /// Otherwise it is killed while the parent is still starting children.
    async fn churn_case(idle: bool) -> Outcome {
        let mut notes = Vec::new();
        let program = match std::env::current_exe() {
            Ok(program) => program,
            Err(error) => {
                return Outcome {
                    verdict: Verdict::Fixture(error.to_string()),
                    notes,
                };
            }
        };
        let mut env: Vec<_> = std::env::vars_os().collect();
        env.push((MODE.into(), "churn-parent".into()));
        let spec = ProcessSpec {
            program: program.into_os_string(),
            args: Vec::new(),
            cwd: None,
            env: Some(env),
            stdin: true,
        };
        let mut process = match OwnedProcess::spawn(&spec, None).await {
            Ok(process) => process,
            Err(error) => {
                return Outcome {
                    verdict: Verdict::Fixture(format!("spawn: {error}")),
                    notes,
                };
            }
        };
        let mut retained: Vec<(u32, OwnedHandle)> = Vec::new();
        let mut settled = false;
        let mut stdout = process.take_stdout();
        let verdict =
            churn_body(&mut process, &mut retained, &mut settled, &mut stdout, idle).await;
        // Cleanup, on every path, as in `settle_case`.
        if !settled {
            process.kill();
            match tokio::time::timeout(BOUND, process.finish()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => notes.push(format!("cleanup settle failed: {error}")),
                Err(_) => notes.push("cleanup settle did not finish".into()),
            }
        }
        for (pid, child) in retained.drain(..) {
            if !wait_bounded(child.as_raw_handle()) {
                // SAFETY: a live handle to a retained child.
                unsafe {
                    TerminateProcess(child.as_raw_handle(), 1);
                }
                if !wait_bounded(child.as_raw_handle()) {
                    notes.push(format!("cleanup: retained child {pid} did not exit"));
                }
            }
        }
        if let Some(mut stdout) = stdout.take() {
            let mut sink = Vec::new();
            match tokio::time::timeout(BOUND, (&mut stdout).take(1 << 20).read_to_end(&mut sink))
                .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => notes.push(format!("cleanup: output pipe error {error}")),
                Err(_) => notes.push("cleanup: the output pipe did not end".into()),
            }
        }
        let cleanup_failed = notes.iter().any(|note| note.starts_with("cleanup"));
        let verdict = match verdict {
            Verdict::Passed if cleanup_failed => Verdict::Fixture("cleanup".into()),
            verdict => verdict,
        };
        Outcome { verdict, notes }
    }

    async fn churn_body(
        process: &mut OwnedProcess,
        retained: &mut Vec<(u32, OwnedHandle)>,
        settled: &mut bool,
        stdout: &mut Option<bake_process::ProcessStdout>,
        idle: bool,
    ) -> Verdict {
        let Some(pipe) = stdout.as_mut() else {
            return Verdict::Fixture("no stdout".into());
        };
        let mut reader = tokio::io::BufReader::new(pipe);
        let mut reported = 0usize;
        loop {
            if retained.len() >= RETAINED && !idle {
                break;
            }
            let mut line = String::new();
            match tokio::time::timeout(BOUND, (&mut reader).take(LINE_LIMIT).read_line(&mut line))
                .await
            {
                Ok(Ok(n)) if n > 0 => {}
                _ => return Verdict::Fixture("the parent stopped reporting children".into()),
            }
            if line.trim() == "done" {
                break;
            }
            let Some(pid) = line
                .trim()
                .strip_prefix("child ")
                .and_then(|pid| pid.parse::<u32>().ok())
            else {
                return Verdict::Fixture(format!("unexpected parent output {line:?}"));
            };
            reported += 1;
            if reported > CHURN {
                return Verdict::Fixture("the parent reported more children than it starts".into());
            }
            if retained.len() >= RETAINED {
                continue;
            }
            let Some(handle) = open(pid) else {
                return Verdict::Fixture(format!("OpenProcess({pid}): {}", last_error()));
            };
            // The parent holds the child's handle while it runs, so the pid
            // named that child if the parent is still running now.
            let parent_running = tokio::select! {
                biased;
                _ = process.wait_exit() => false,
                () = std::future::ready(()) => true,
            };
            if !parent_running {
                return Verdict::Fixture("the parent exited while children were retained".into());
            }
            retained.push((pid, handle));
        }
        if retained.len() < RETAINED {
            return Verdict::Fixture(format!("only {} children retained", retained.len()));
        }
        // Killed while it is still starting children (or, idle, after its
        // whole churn), with exiting ones among them, as an MCP shutdown
        // or a tool abort does.
        process.kill();
        let report = match tokio::time::timeout(BOUND, process.finish()).await {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => return Verdict::Fixture(format!("finish: {error}")),
            Err(_) => return Verdict::Incomplete(format!("finish did not complete in {BOUND:?}")),
        };
        *settled = true;
        if report.tree != TreeState::Gone {
            return Verdict::Failed(format!("the killed tree settled as {:?}", report.tree));
        }
        // Each checked once, with no wait.
        let mut unsignalled = Vec::new();
        for (pid, handle) in retained.iter() {
            match signalled(handle.as_raw_handle()) {
                Some(true) => {}
                Some(false) => unsignalled.push(*pid),
                None => return Verdict::Fixture("a retained handle could not be checked".into()),
            }
        }
        if unsignalled.is_empty() {
            Verdict::Passed
        } else {
            Verdict::Failed(format!(
                "finish reported Gone with retained children {unsignalled:?} unsignalled"
            ))
        }
    }
}
