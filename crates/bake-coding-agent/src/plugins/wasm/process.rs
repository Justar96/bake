//! The helper process as the parent sees it: how it is created, its three
//! pipes, waiting for its exit, and killing it.
//!
//! - **Unix:** Tokio's `Command` with a cleared environment (on macOS
//!   holding only `MallocMediumZone=0` and `MallocMaxMagazines=1`; see
//!   `os_limit`), `/` as the
//!   working directory, piped standard streams, and `setsid` before `exec`,
//!   so the helper has no controlling terminal. Only the three pipes are
//!   inherited (Rust opens every other descriptor close-on-exec).
//! - **Windows:** `CreateProcessW` with `PROC_THREAD_ATTRIBUTE_JOB_LIST`,
//!   so the helper is created inside its Job: it runs no instruction
//!   outside it, and no window exists between creation and assignment in
//!   which a parent's death would leave it unconfined. The Job has
//!   kill-on-close, a one-process limit, and the committed-memory budget.
//!   `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` restricts inheritance to the
//!   helper's three pipe ends. See [`windows`].

use std::ffi::OsString;
use std::path::Path;

use tokio::io::{AsyncRead, AsyncWrite};

/// The parent's end of the helper's standard input.
pub(super) type PipeIn = Box<dyn AsyncWrite + Send + Unpin>;
/// The parent's end of the helper's standard output or error.
pub(super) type PipeOut = Box<dyn AsyncRead + Send + Unpin>;

/// How the helper ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// A spawned helper and its pipes.
pub(super) struct Spawned {
    pub process: Process,
    pub stdin: PipeIn,
    pub stdout: PipeOut,
    pub stderr: PipeOut,
}

/// Spawns `program` with `args` for a helper whose memory budget is
/// `budget` bytes. On error no process exists. The process is created
/// after this function's last await (on Windows the pipes are connected
/// first), so a caller that stores the result before its next await owns
/// the helper from its creation; dropping this future never loses one.
pub(super) async fn spawn(
    program: &Path,
    args: &[OsString],
    budget: u64,
) -> Result<Spawned, String> {
    imp::spawn(program, args, budget).await
}

#[cfg(unix)]
mod imp {
    use std::ffi::OsString;
    use std::path::Path;
    use std::process::Stdio;

    use tokio::process::{Child, Command};

    use super::{Exit, Spawned};

    /// The helper process: Tokio's child, which `kill_on_drop` kills if the
    /// owner is dropped before it was reaped.
    #[derive(Debug)]
    pub(in super::super) struct Process {
        child: Child,
        pid: Option<u32>,
    }

    pub(super) async fn spawn(
        program: &Path,
        args: &[OsString],
        _budget: u64,
    ) -> Result<Spawned, String> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // libmalloc's medium allocator reserves 128 MiB of address space per
        // region and CPU magazine, which the helper's `RLIMIT_AS` budget
        // cannot absorb. The helper refuses to start without this setting
        // (`os_limit::check_medium_allocator`); the hard limit is unchanged.
        #[cfg(target_os = "macos")]
        cmd.env("MallocMediumZone", "0");
        // libmalloc picks a tiny and small magazine by the CPU a thread is
        // running on, and each magazine maps its own 1 MiB and 8 MiB
        // regions; one magazine keeps that from growing with CPU spread.
        #[cfg(target_os = "macos")]
        cmd.env("MallocMaxMagazines", "1");
        // SAFETY: the closure runs in the forked child before `exec` and
        // calls only `setsid`, which is async-signal-safe, and reads errno.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|error| format!("{}: {error}", program.display()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            // Unreachable with piped streams; the child is killed on drop.
            return Err("the helper's pipes are missing".to_owned());
        };
        let pid = child.id();
        Ok(Spawned {
            process: Process { child, pid },
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
        })
    }

    impl Process {
        pub(in super::super) fn id(&self) -> Option<u32> {
            self.pid
        }

        /// The exit, if the helper has exited; reaps it then.
        pub(in super::super) fn try_exit(&mut self) -> Option<Exit> {
            self.child.try_wait().ok().flatten().map(exit)
        }

        /// Waits for the exit. Cancel-safe, and the status is kept, so a
        /// later call returns it again.
        pub(in super::super) async fn wait(&mut self) -> std::io::Result<Exit> {
            self.child.wait().await.map(exit)
        }

        /// Sends the kill; a helper that already exited is unaffected.
        pub(in super::super) fn kill(&mut self) {
            let _ = self.child.start_kill();
        }

        pub(in super::super) fn kill_switch(&self) -> KillSwitch {
            KillSwitch { pid: self.pid }
        }
    }

    fn exit(status: std::process::ExitStatus) -> Exit {
        use std::os::unix::process::ExitStatusExt;
        Exit {
            code: status.code(),
            signal: status.signal(),
        }
    }

    /// Kills the helper without borrowing its owner, so a dropped future
    /// can. Armed only while the owner holds the unreaped child, so the
    /// pid cannot have been reused.
    pub(in super::super) struct KillSwitch {
        pid: Option<u32>,
    }

    impl KillSwitch {
        pub(in super::super) fn kill(&self) {
            if let Some(pid) = self.pid.and_then(|pid| libc::pid_t::try_from(pid).ok())
                && pid > 1
            {
                // SAFETY: plain syscall on an unreaped child (see above).
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(windows)]
pub(super) use windows as imp;

#[cfg(windows)]
pub(super) mod windows {
    //! Windows: the helper is created inside its Job by `CreateProcessW`
    //! with `PROC_THREAD_ATTRIBUTE_JOB_LIST` (Windows 10 and later), and
    //! its standard streams are named pipes whose parent ends are Tokio
    //! named-pipe servers.
    //!
    //! - The Job is created first, with kill-on-close, a one-process
    //!   limit, die-on-unhandled-exception, and the committed-memory
    //!   budget. Its handle is not inheritable and stays in the parent: the
    //!   helper dies when the parent's last handle closes, the parent's
    //!   death included. Creation fails as a whole if the Job cannot be
    //!   applied, so no process exists outside it.
    //! - Each pipe has a random name, one instance, the first-instance
    //!   flag (so another process cannot have created it first), and
    //!   remote clients refused. The child ends are opened synchronously,
    //!   made inheritable only for the `CreateProcessW` call, and named in
    //!   `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, so the helper inherits those
    //!   three handles and no other. Another thread of this process that
    //!   creates a process with inheritance and without a handle list
    //!   during that call could also inherit them; the parent would then
    //!   see the helper's output end late, which the bounded drain reports.
    //! - The environment holds only `SystemRoot`, which Windows needs to
    //!   load system libraries; the working directory is the executable's.
    //! - Exit is observed by polling the process handle every 10 ms while
    //!   the owner waits, so no callback or thread outlives a wait.
    //!
    //! This code is type-checked and linted for Windows but has not run;
    //! Windows CI owns that evidence. The only `unsafe` code is FFI, each
    //! call with its contract.

    use std::ffi::{OsStr, OsString, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
        InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::{Exit, Spawned};

    /// The exit code `TerminateJobObject` and `TerminateProcess` give.
    const KILLED: u32 = 1;
    const POLL: Duration = Duration::from_millis(10);
    const PIPE_BUFFER: u32 = 64 * 1024;

    fn last_error(what: &str) -> String {
        format!("{what} failed: {}", std::io::Error::last_os_error())
    }

    fn wide(text: &OsStr) -> Result<Vec<u16>, String> {
        let units: Vec<u16> = text.encode_wide().collect();
        if units.contains(&0) {
            return Err(format!("{} contains a NUL character", text.display()));
        }
        Ok(units.into_iter().chain([0]).collect())
    }

    /// The helper's Job.
    #[derive(Debug)]
    pub(in super::super) struct Job(OwnedHandle);

    impl Job {
        fn new(budget: u64) -> Result<Self, String> {
            // SAFETY: null attributes (not inheritable) and no name.
            let raw = unsafe { CreateJobObjectW(null(), null()) };
            if raw.is_null() {
                return Err(last_error("CreateJobObjectW"));
            }
            // SAFETY: a fresh handle this function exclusively owns.
            let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
            // SAFETY: a zeroed struct is a valid starting value.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            limits.ProcessMemoryLimit =
                usize::try_from(budget).map_err(|_| "the budget does not fit".to_owned())?;
            // SAFETY: `limits` has the size passed; the handle is live.
            if unsafe {
                SetInformationJobObject(
                    job.0.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(last_error("SetInformationJobObject"));
            }
            Ok(job)
        }

        fn terminate(&self) {
            // Best effort: the result is not checked, and success does not
            // mean the processes have exited. Only an observed exit
            // (`Process::try_exit`) establishes that.
            // SAFETY: a live Job handle owned by `self`.
            unsafe {
                TerminateJobObject(self.0.as_raw_handle(), KILLED);
            }
        }
    }

    /// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop, as in
    /// `bake-sandbox`'s Windows runner.
    struct Attributes(Vec<u64>);

    impl Attributes {
        fn new(count: u32) -> Result<Self, String> {
            let mut size = 0usize;
            // SAFETY: a size query; it fails and reports the size.
            unsafe {
                InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
            }
            if size == 0 {
                return Err(last_error("InitializeProcThreadAttributeList"));
            }
            let mut buffer = vec![0u64; size.div_ceil(8)];
            // SAFETY: `buffer` holds at least `size` bytes, 8-aligned.
            if unsafe {
                InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
            } == 0
            {
                return Err(last_error("InitializeProcThreadAttributeList"));
            }
            Ok(Self(buffer))
        }

        fn pointer(&mut self) -> *mut c_void {
            self.0.as_mut_ptr().cast()
        }

        /// Sets `attribute` to `size` bytes at `value`, which the caller
        /// keeps alive until the list is dropped.
        fn set(
            &mut self,
            attribute: usize,
            value: *const c_void,
            size: usize,
        ) -> Result<(), String> {
            // SAFETY: the list is initialized; the caller keeps `value` alive.
            if unsafe {
                UpdateProcThreadAttribute(
                    self.pointer(),
                    0,
                    attribute,
                    value,
                    size,
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(last_error("UpdateProcThreadAttribute"));
            }
            Ok(())
        }
    }

    impl Drop for Attributes {
        fn drop(&mut self) {
            // SAFETY: initialized in `new`, deleted once.
            unsafe { DeleteProcThreadAttributeList(self.pointer()) }
        }
    }

    /// A pipe with a unique name: the parent's server end and the child's
    /// end, opened synchronously and not yet inheritable.
    fn pipe(child_reads: bool) -> Result<(NamedPipeServer, OwnedHandle), String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|error| format!("getrandom failed: {error}"))?;
        let name = format!(
            r"\\.\pipe\bake-wasm-plugin-{}-{}-{:016x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            u64::from_le_bytes(nonce)
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .pipe_mode(PipeMode::Byte)
            .access_inbound(!child_reads)
            .access_outbound(child_reads)
            .in_buffer_size(PIPE_BUFFER)
            .out_buffer_size(PIPE_BUFFER)
            .create(&name)
            .map_err(|error| format!("creating pipe {name}: {error}"))?;
        let wide_name = wide(OsStr::new(&name))?;
        // SAFETY: a NUL-terminated name; no security attributes, so the
        // handle is not inheritable; a synchronous handle (no overlapped
        // flag), as the helper's standard streams expect.
        let raw = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                if child_reads {
                    GENERIC_READ
                } else {
                    GENERIC_WRITE
                },
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(last_error("CreateFileW (pipe client)"));
        }
        // SAFETY: a fresh handle this function exclusively owns.
        let client = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok((server, client))
    }

    fn inheritable(handle: &OwnedHandle, inherit: bool) -> Result<(), String> {
        // SAFETY: a live handle owned by the caller.
        if unsafe {
            SetHandleInformation(
                handle.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                if inherit { HANDLE_FLAG_INHERIT } else { 0 },
            )
        } == 0
        {
            return Err(last_error("SetHandleInformation"));
        }
        Ok(())
    }

    pub(super) async fn spawn(
        program: &Path,
        args: &[OsString],
        budget: u64,
    ) -> Result<Spawned, String> {
        let job = Job::new(budget)?;
        let (stdin, child_stdin) = pipe(true)?;
        let (stdout, child_stdout) = pipe(false)?;
        let (stderr, child_stderr) = pipe(false)?;
        // Each client end is already open, so `ConnectNamedPipe` reports
        // the pipe connected at once; mio schedules its overlapped reads
        // only once connected. This is the last await: the process is
        // created below without another.
        for server in [&stdin, &stdout, &stderr] {
            server
                .connect()
                .await
                .map_err(|error| format!("connecting a helper pipe: {error}"))?;
        }

        let mut argv: Vec<Vec<u16>> = vec![program.as_os_str().encode_wide().collect()];
        argv.extend(args.iter().map(|arg| arg.encode_wide().collect()));
        if argv.iter().any(|arg| arg.contains(&0)) {
            return Err("the helper command contains a NUL character".to_owned());
        }
        let mut command_line = bake_sandbox::backends::windows_acl::build_command_line(&argv);
        command_line.push(0);
        // The program is passed as the application name, so the command
        // line's first word is never searched for.
        let application = wide(program.as_os_str())?;
        let directory = program
            .parent()
            .map(|dir| wide(dir.as_os_str()))
            .transpose()?;
        let mut environment: Vec<u16> = Vec::new();
        if let Some(root) = std::env::var_os("SystemRoot") {
            environment.extend(OsStr::new("SystemRoot=").encode_wide());
            let value: Vec<u16> = root.encode_wide().collect();
            if !value.contains(&0) {
                environment.extend(value);
            }
            environment.push(0);
        } else {
            environment.push(0);
        }
        environment.push(0);

        let inherited: [HANDLE; 3] = [
            child_stdin.as_raw_handle(),
            child_stdout.as_raw_handle(),
            child_stderr.as_raw_handle(),
        ];
        let job_list: [HANDLE; 1] = [job.0.as_raw_handle()];
        let mut attributes = Attributes::new(2)?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            job_list.as_ptr().cast(),
            size_of_val(&job_list),
        )?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            inherited.as_ptr().cast(),
            size_of_val(&inherited),
        )?;
        // SAFETY: a zeroed STARTUPINFOEXW is valid before its fields are set.
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited[0];
        startup.StartupInfo.hStdOutput = inherited[1];
        startup.StartupInfo.hStdError = inherited[2];
        startup.lpAttributeList = attributes.pointer();
        // SAFETY: a zeroed PROCESS_INFORMATION is valid output space.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // The handle list's entries must be inheritable for the call.
        for handle in [&child_stdin, &child_stdout, &child_stderr] {
            inheritable(handle, true)?;
        }
        // SAFETY: every string is NUL-terminated (the environment block
        // double-NUL-terminated); `command_line` is mutable as the API
        // requires; the attribute values (`job_list`, `inherited`) and the
        // handles they name outlive the call.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                environment.as_ptr().cast(),
                directory.as_ref().map_or(null(), |dir| dir.as_ptr()),
                &startup.StartupInfo,
                &mut info,
            )
        };
        let created_error = (created == 0).then(|| last_error("CreateProcessW"));
        drop(attributes);
        // The child ends belong to the helper now (or to nobody on
        // failure); the parent closes its copies either way.
        drop((child_stdin, child_stdout, child_stderr));
        if let Some(error) = created_error {
            return Err(format!("{}: {error}", program.display()));
        }
        if info.hProcess.is_null() {
            // Unreachable on success; closing the Job (kill-on-close)
            // sends the kill to any process in it.
            return Err("CreateProcessW returned no process handle".to_owned());
        }
        // SAFETY: fresh handles from CreateProcessW that this function owns.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        if !info.hThread.is_null() {
            // SAFETY: as above; the thread handle is not needed.
            drop(unsafe { OwnedHandle::from_raw_handle(info.hThread) });
        }
        Ok(Spawned {
            process: Process {
                handle: process,
                pid: info.dwProcessId,
                job: Arc::new(job),
                exit: None,
            },
            stdin: Box::new(Pipe(stdin)),
            stdout: Box::new(Pipe(stdout)),
            stderr: Box::new(Pipe(stderr)),
        })
    }

    /// A pipe server end whose broken-pipe error, the child having closed
    /// its end, reads as end of file.
    struct Pipe(NamedPipeServer);

    impl AsyncRead for Pipe {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            match std::pin::Pin::new(&mut self.0).poll_read(cx, buf) {
                std::task::Poll::Ready(Err(error))
                    if error.kind() == std::io::ErrorKind::BrokenPipe =>
                {
                    std::task::Poll::Ready(Ok(()))
                }
                other => other,
            }
        }
    }

    impl AsyncWrite for Pipe {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    /// The helper process, its handle, and its Job.
    #[derive(Debug)]
    pub(in super::super) struct Process {
        handle: OwnedHandle,
        pid: u32,
        job: Arc<Job>,
        exit: Option<Exit>,
    }

    impl Process {
        pub(in super::super) fn id(&self) -> Option<u32> {
            Some(self.pid)
        }

        pub(in super::super) fn try_exit(&mut self) -> Option<Exit> {
            if self.exit.is_none() {
                // SAFETY: a live process handle; a zero timeout only polls.
                if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
                    let mut code = 0u32;
                    // SAFETY: a live handle and valid output space.
                    let read =
                        unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) };
                    self.exit = Some(Exit {
                        code: (read != 0).then_some(code as i32),
                        signal: None,
                    });
                }
            }
            self.exit
        }

        /// Polls the handle every 10 ms; cancel-safe, and the exit is kept.
        pub(in super::super) async fn wait(&mut self) -> std::io::Result<Exit> {
            loop {
                if let Some(exit) = self.try_exit() {
                    return Ok(exit);
                }
                tokio::time::sleep(POLL).await;
            }
        }

        pub(in super::super) fn kill(&mut self) {
            self.job.terminate();
            // Best effort as well, after the Job's: the result is not
            // checked; the owner keeps waiting for the observed exit.
            // SAFETY: a live process handle owned by `self`.
            unsafe {
                TerminateProcess(self.handle.as_raw_handle(), KILLED);
            }
        }

        pub(in super::super) fn kill_switch(&self) -> KillSwitch {
            KillSwitch {
                job: Arc::clone(&self.job),
            }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // Dropping the owner sends a best-effort kill and awaits
            // nothing: the Job's last handle may be a switch's, so terminate
            // rather than rely on close. When the last Job handle closes,
            // kill-on-close applies as well.
            if self.exit.is_none() {
                self.job.terminate();
            }
        }
    }

    /// Kills the helper through its Job without borrowing its owner.
    pub(in super::super) struct KillSwitch {
        job: Arc<Job>,
    }

    impl KillSwitch {
        pub(in super::super) fn kill(&self) {
            self.job.terminate();
        }
    }
}

pub(super) use imp::{KillSwitch, Process};
