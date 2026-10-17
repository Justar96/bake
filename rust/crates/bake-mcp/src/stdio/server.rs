//! The server process and its tree, per platform. This is cleanup
//! ownership, not D24 containment: a descendant that starts its own session
//! or process group (Unix) or breaks away from the Job (Windows, where the
//! Job allows it) escapes it, and nothing here limits what the server can
//! read, write, or reach.
//!
//! Unix: the server leads a new process group. Its exit is observed with
//! `waitid(WNOWAIT)`, which leaves it unreaped: while the leader is
//! unreaped (running or a zombie) its pid, and so the group's id, cannot be
//! reused, so signalling the group reaches only this server's tree. The tree
//! is killed before the leader is reaped, and no group signal is sent once
//! it is reaped or found reaped by someone else. The kill is not awaited:
//! group membership is not yet observed before the reap (an open gap).
//!
//! Windows: the server is created inside a new kill-on-close Job
//! (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), so it runs no instruction outside it,
//! and inherits only its three pipe ends (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
//! The Job allows no breakaway. The owner holds the process and Job handles,
//! so neither id can be reused while it can still signal them. Exit and the
//! Job's emptiness are observed by polling those handles; no callback or
//! thread-pool wait holds owner state. Our pipe ends are overlapped
//! named-pipe servers driven by Tokio's I/O driver, not blocking-pool
//! threads; every other end is held only by processes in the Job, so once
//! the Job is empty every pending operation on them completes or fails.
//!
//! The only `unsafe` code in the crate is in this module, each call with its
//! contract.
#![allow(unsafe_code)]

use std::io;

use super::{StdioCommand, TreeEnd};

/// A spawned server's pipes, our ends. They are always present after a
/// successful spawn; `Option` keeps the impossible case total without a
/// panic.
pub(super) struct Pipes {
    pub stdin: Option<PipeWriter>,
    pub stdout: Option<PipeReader>,
    pub stderr: Option<ErrorReader>,
}

#[cfg(unix)]
pub(super) type PipeWriter = tokio::process::ChildStdin;
#[cfg(unix)]
pub(super) type PipeReader = tokio::process::ChildStdout;
#[cfg(unix)]
pub(super) type ErrorReader = tokio::process::ChildStderr;
#[cfg(windows)]
pub(super) type PipeWriter = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
pub(super) type PipeReader = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
pub(super) type ErrorReader = tokio::net::windows::named_pipe::NamedPipeServer;

#[cfg(unix)]
pub(super) use unix::Server;
#[cfg(windows)]
pub(super) use windows::Server;

/// Spawn `command` with its tree owned from creation. Nothing is left
/// running when this returns an error.
pub(super) fn spawn(command: &StdioCommand) -> io::Result<(Server, Pipes)> {
    Server::spawn(command)
}

/// Whether the leader's identity is still held, so the tree can be signalled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Identity {
    /// Running, or exited and not yet reaped.
    Held,
    /// Reaped by this owner.
    Reaped,
    /// Reaped by someone else (Unix: `waitid` reported `ECHILD`). The tree
    /// can no longer be signalled safely, and was not.
    #[cfg_attr(windows, allow(dead_code))]
    Lost,
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::process::{ExitStatus, Stdio};

    use tokio::process::{Child, Command};
    use tokio::signal::unix::{Signal, SignalKind, signal};

    use super::{Identity, Pipes, TreeEnd};
    use crate::stdio::StdioCommand;

    #[derive(Debug)]
    pub(in crate::stdio) struct Server {
        child: Child,
        pid: libc::pid_t,
        /// Every `SIGCHLD` this process receives; registered before the
        /// spawn, so the server's exit cannot be missed.
        sigchld: Signal,
        identity: Identity,
    }

    impl Server {
        pub(super) fn spawn(command: &StdioCommand) -> io::Result<(Self, Pipes)> {
            let sigchld = signal(SignalKind::child())?;
            let mut process = Command::new(&command.program);
            process
                .args(&command.args)
                .env_clear()
                .envs(command.env.iter().map(|(name, value)| (name, value)))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // The leader leads a new group whose id is its pid, set in
                // the child before `exec`, so no descendant starts outside it.
                .process_group(0);
            // No `kill_on_drop`: Tokio would signal the leader's pid even
            // after something else reaped it. `Drop` kills the group (the
            // leader with it) only while the identity holds, and Tokio's
            // orphan reaper then reaps the leader.
            if let Some(dir) = &command.current_dir {
                process.current_dir(dir);
            }
            let mut child = process.spawn()?;
            let pipes = Pipes {
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                stderr: child.stderr.take(),
            };
            // Tokio reports no id only once it has reaped the child, which
            // it has not; a pid that does not fit, or that names group 0 or
            // 1, cannot be signalled as a group at all.
            let pid = child
                .id()
                .and_then(|pid| libc::pid_t::try_from(pid).ok())
                .filter(|pid| *pid > 1);
            let Some(pid) = pid else {
                // Unreachable for a child Tokio has not polled; kill it
                // directly rather than leave it running.
                let _ = child.start_kill();
                return Err(io::Error::other("the server has no usable process id"));
            };
            Ok((
                Self {
                    child,
                    pid,
                    sigchld,
                    identity: Identity::Held,
                },
                pipes,
            ))
        }

        pub(in crate::stdio) fn pid(&self) -> u32 {
            self.pid.unsigned_abs()
        }

        /// Whether the leader has exited (or its identity is gone), without
        /// reaping it.
        fn has_exited(&mut self) -> bool {
            if self.identity != Identity::Held {
                return true;
            }
            loop {
                // SAFETY: a zeroed `siginfo_t` is valid output space.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                // SAFETY: `info` is valid for writes; plain syscall.
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        self.pid.unsigned_abs(),
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                    )
                };
                if result == 0 {
                    // With `WNOHANG` and nothing to report, `si_pid` stays 0.
                    // SAFETY: `waitid` filled `info` (or left it zeroed).
                    return unsafe { info.si_pid() } != 0;
                }
                match io::Error::last_os_error().raw_os_error() {
                    Some(libc::EINTR) => continue,
                    // Someone else reaped it: the pid may already be reused.
                    Some(libc::ECHILD) => {
                        self.identity = Identity::Lost;
                        return true;
                    }
                    // Any other failure: report it as running, so callers
                    // escalate; signals still go only while identity holds.
                    _ => return false,
                }
            }
        }

        /// Resolves once the leader has exited; it stays unreaped.
        /// Cancel-safe.
        pub(in crate::stdio) async fn exited(&mut self) {
            while !self.has_exited() {
                if self.sigchld.recv().await.is_none() {
                    // The signal driver is gone (the runtime is shutting
                    // down); nothing will wake this again.
                    std::future::pending::<()>().await;
                }
            }
        }

        fn signal_tree(&mut self, signal: libc::c_int) {
            // Checked first, so a leader reaped elsewhere since the last
            // look is never signalled as a group.
            self.has_exited();
            if self.identity == Identity::Held {
                // SAFETY: plain syscall; `pid` is above 1 and names a group
                // whose leader this owner has not reaped.
                unsafe {
                    libc::kill(-self.pid, signal);
                }
            }
        }

        /// Ask the tree to stop (`SIGTERM` to the group).
        pub(in crate::stdio) fn terminate(&mut self) {
            self.signal_tree(libc::SIGTERM);
        }

        /// Kill every process still in the tree (`SIGKILL` to the group).
        pub(in crate::stdio) fn kill_tree(&mut self) {
            self.signal_tree(libc::SIGKILL);
        }

        /// Reap the leader after [`exited`](Self::exited): send `SIGKILL` to
        /// its group while its identity is still held, then reap. The kill
        /// is not awaited. `None` if
        /// the leader was reaped elsewhere.
        pub(in crate::stdio) fn reap(&mut self) -> Option<ExitStatus> {
            if self.identity != Identity::Held {
                return None;
            }
            self.kill_tree();
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.identity = Identity::Reaped;
                    Some(status)
                }
                // Not exited: the caller did not wait for `exited`.
                Ok(None) => None,
                Err(_) => {
                    self.identity = Identity::Lost;
                    None
                }
            }
        }
    }

    impl Server {
        /// Not yet implemented on Unix: returns at once. Awaiting the
        /// group's members would have to happen before the leader is reaped
        /// (Linux `/proc`, macOS `proc_listpids`); until then
        /// [`TreeEnd::Killed`] reports a kill request, not an observed end.
        pub(in crate::stdio) async fn tree_ended(&mut self) {}

        /// How the tree ended, once [`reap`](Self::reap) ran.
        pub(in crate::stdio) fn tree(&self) -> TreeEnd {
            match self.identity {
                Identity::Reaped => TreeEnd::Killed,
                Identity::Held | Identity::Lost => TreeEnd::NotSignalled,
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // Only while the leader's identity holds; the leader is in its
            // group, so it dies too, and `child`'s drop leaves the reap to
            // Tokio's orphan reaper.
            self.kill_tree();
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, c_void};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
    use std::os::windows::process::ExitStatusExt;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::net::windows::named_pipe::NamedPipeServer;
    use windows_sys::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND,
        PIPE_ACCESS_OUTBOUND,
    };
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
        InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::{Identity, Pipes, TreeEnd};
    use crate::stdio::StdioCommand;

    /// How often exit and the Job's emptiness are polled.
    const POLL: Duration = Duration::from_millis(10);

    #[derive(Debug)]
    pub(in crate::stdio) struct Server {
        process: OwnedHandle,
        job: OwnedHandle,
        pid: u32,
        identity: Identity,
        /// Set once the Job was seen with no active process.
        job_empty: bool,
    }

    impl Server {
        pub(super) fn spawn(command: &StdioCommand) -> io::Result<(Self, Pipes)> {
            let mut units = vec![wide_arg(&command.program)?];
            for arg in &command.args {
                units.push(wide_arg(arg)?);
            }
            let mut command_line = command_line(&units);
            command_line.push(0);
            // An absolute program is the application name, so the search
            // never reaches the current directory. A bare name is searched
            // as `CreateProcessW` searches; pass a path to be sure.
            let application = if Path::new(&command.program).is_absolute() {
                Some(wide(&command.program)?)
            } else {
                None
            };
            let environment = environment_block(&command.env)?;
            let current_dir = command
                .current_dir
                .as_deref()
                .map(|dir| wide(dir.as_os_str()))
                .transpose()?;
            let job = kill_on_close_job()?;
            // The child's ends are made inheritable; `HANDLE_LIST` gives the
            // server exactly these three. Our ends stay non-inheritable.
            let (stdin_ours, stdin_child) = pipe(Direction::ToChild)?;
            let (stdout_ours, stdout_child) = pipe(Direction::FromChild)?;
            let (stderr_ours, stderr_child) = pipe(Direction::FromChild)?;
            let child_ends: [OwnedHandle; 3] = [stdin_child, stdout_child, stderr_child];
            for handle in &child_ends {
                // SAFETY: a live handle this function owns.
                if unsafe {
                    SetHandleInformation(
                        handle.as_raw_handle(),
                        HANDLE_FLAG_INHERIT,
                        HANDLE_FLAG_INHERIT,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            let inherited: [HANDLE; 3] = [
                child_ends[0].as_raw_handle(),
                child_ends[1].as_raw_handle(),
                child_ends[2].as_raw_handle(),
            ];
            let job_list: [HANDLE; 1] = [job.as_raw_handle()];
            let mut attributes = Attributes::new(2)?;
            attributes.set(
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                job_list.as_ptr().cast(),
                size_of_val(&job_list),
            )?;
            attributes.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
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
            // SAFETY: every string is NUL-terminated (the environment block
            // double-NUL), `command_line` is mutable as required, and the
            // attribute values and handles outlive the call.
            let created = unsafe {
                CreateProcessW(
                    application.as_ref().map_or(null(), |name| name.as_ptr()),
                    command_line.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                    environment.as_ptr().cast(),
                    current_dir.as_ref().map_or(null(), |dir| dir.as_ptr()),
                    &startup.StartupInfo,
                    &mut info,
                )
            };
            if created == 0 {
                return Err(io::Error::last_os_error());
            }
            drop(attributes);
            // The child holds its own copies now.
            drop(child_ends);
            // SAFETY: fresh handles from `CreateProcessW`, owned only here.
            let thread = (!info.hThread.is_null())
                .then(|| unsafe { OwnedHandle::from_raw_handle(info.hThread) });
            drop(thread);
            if info.hProcess.is_null() {
                // The process exists inside the Job; dropping `job` here
                // (kill on close) ends it.
                return Err(io::Error::other(
                    "CreateProcessW returned no process handle",
                ));
            }
            // SAFETY: as above.
            let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
            let pipes = Pipes {
                stdin: Some(stdin_ours),
                stdout: Some(stdout_ours),
                stderr: Some(stderr_ours),
            };
            Ok((
                Self {
                    process,
                    job,
                    pid: info.dwProcessId,
                    identity: Identity::Held,
                    job_empty: false,
                },
                pipes,
            ))
        }

        pub(in crate::stdio) fn pid(&self) -> u32 {
            self.pid
        }

        fn has_exited(&self) -> bool {
            // SAFETY: a live process handle; a zero timeout only checks.
            unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
        }

        /// Resolves once the server has exited. Cancel-safe: it polls the
        /// process handle and holds nothing between polls.
        pub(in crate::stdio) async fn exited(&mut self) {
            while !self.has_exited() {
                tokio::time::sleep(POLL).await;
            }
        }

        /// The Job's active process count; `None` if it cannot be read.
        fn active_processes(&self) -> Option<u32> {
            // SAFETY: a zeroed struct is valid output space.
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: a live Job handle and an out buffer of the size passed.
            let queried = unsafe {
                QueryInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    std::ptr::addr_of_mut!(info).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    null_mut(),
                )
            };
            (queried != 0).then_some(info.ActiveProcesses)
        }

        /// Resolves once the Job has no active process: terminating a Job
        /// only requests it, and its processes end asynchronously.
        /// Cancel-safe. An unreadable count is waited on, not assumed empty.
        pub(in crate::stdio) async fn tree_ended(&mut self) {
            while !self.job_empty {
                if self.active_processes() == Some(0) {
                    self.job_empty = true;
                    return;
                }
                tokio::time::sleep(POLL).await;
            }
        }

        /// How the tree ended, once [`tree_ended`](Self::tree_ended) resolved.
        pub(in crate::stdio) fn tree(&self) -> TreeEnd {
            if self.job_empty {
                TreeEnd::Empty
            } else {
                TreeEnd::Killed
            }
        }

        /// Windows has no graceful request; the caller escalates.
        pub(in crate::stdio) fn terminate(&mut self) {}

        /// Terminate every process in the Job. The Job handle is held, so
        /// this reaches no other process.
        pub(in crate::stdio) fn kill_tree(&mut self) {
            // SAFETY: a valid Job handle this owner holds.
            unsafe {
                TerminateJobObject(self.job.as_raw_handle(), 1);
            }
        }

        /// The exit code, once [`exited`](Self::exited) resolved. The Job is
        /// terminated first; that only requests its processes' end, which
        /// [`tree_ended`](Self::tree_ended) awaits.
        pub(in crate::stdio) fn reap(&mut self) -> Option<ExitStatus> {
            if !self.has_exited() {
                return None;
            }
            self.kill_tree();
            let mut code = 0_u32;
            // SAFETY: a live process handle and a valid out pointer.
            if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                return None;
            }
            self.identity = Identity::Reaped;
            Some(ExitStatus::from_raw(code))
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // Closing the Job's last handle kills whatever is still in it.
            if !self.job_empty {
                self.kill_tree();
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Direction {
        ToChild,
        FromChild,
    }

    /// One pipe: our end an overlapped named-pipe server, the child's end a
    /// synchronous client handle (made inheritable by the caller).
    ///
    /// The server handle is created raw and handed to Tokio only after the
    /// client has opened it. Tokio's (Mio's) registration starts the
    /// overlapped read at once; on a pipe still listening for its client
    /// that read fails with `ERROR_PIPE_LISTENING`, nothing restarts it but
    /// an explicit connect, and reads would never wake. A client that opens
    /// the pipe before `ConnectNamedPipe` is connected, so registering
    /// afterwards starts I/O on a connected pipe without any await.
    ///
    /// The name is unique to this process, and `FILE_FLAG_FIRST_PIPE_INSTANCE`
    /// refuses one that already exists; with a single instance, opened at
    /// once, no other client gets a slot (a racing client makes our open fail,
    /// so the spawn fails closed). Remote clients are rejected.
    fn pipe(direction: Direction) -> io::Result<(NamedPipeServer, OwnedHandle)> {
        const BUFFER: u32 = 65_536;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            r"\\.\pipe\bake-mcp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let wide_name = wide(OsStr::new(&name))?;
        let to_child = matches!(direction, Direction::ToChild);
        let access = if to_child {
            PIPE_ACCESS_OUTBOUND
        } else {
            PIPE_ACCESS_INBOUND
        };
        // SAFETY: `wide_name` is NUL-terminated; null security attributes
        // give a non-inheritable handle with the default descriptor.
        let raw = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER,
                BUFFER,
                0,
                null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle owned only here.
        let server = unsafe { OwnedHandle::from_raw_handle(raw) };
        // Connects the single instance (or fails, closing `server`).
        let client = std::fs::OpenOptions::new()
            .read(to_child)
            .write(!to_child)
            .open(&name)?;
        // SAFETY: ownership of the connected server handle moves to Tokio,
        // which closes it (also when registration fails).
        let server = unsafe { NamedPipeServer::from_raw_handle(server.into_raw_handle())? };
        Ok((server, OwnedHandle::from(client)))
    }

    fn kill_on_close_job() -> io::Result<OwnedHandle> {
        // SAFETY: null attributes (not inheritable) and no name.
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh handle owned only here.
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: a zeroed struct is a valid starting value.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is valid for the size passed; `job` is open.
        let set = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop (as in
    /// `bake-sandbox`'s Windows runner).
    struct Attributes(Vec<u64>);

    impl Attributes {
        fn new(count: u32) -> io::Result<Self> {
            let mut size = 0_usize;
            // SAFETY: a size query; it fails and reports the size.
            unsafe {
                InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
            }
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0_u64; size.div_ceil(8)];
            // SAFETY: `buffer` holds at least `size` bytes.
            if unsafe {
                InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(buffer))
        }

        fn pointer(&mut self) -> *mut c_void {
            self.0.as_mut_ptr().cast()
        }

        /// The caller keeps the `size` bytes at `value` alive until the
        /// process is created.
        fn set(&mut self, attribute: u32, value: *const c_void, size: usize) -> io::Result<()> {
            // SAFETY: the list is initialized; the caller keeps `value` alive.
            if unsafe {
                UpdateProcThreadAttribute(
                    self.pointer(),
                    0,
                    attribute as usize,
                    value,
                    size,
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Attributes {
        fn drop(&mut self) {
            // SAFETY: the list was initialized and is deleted once.
            unsafe { DeleteProcThreadAttributeList(self.pointer()) }
        }
    }

    fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
        let mut units = wide_arg(text)?;
        units.push(0);
        Ok(units)
    }

    fn wide_arg(text: &OsStr) -> io::Result<Vec<u16>> {
        let units: Vec<u16> = text.encode_wide().collect();
        if units.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a NUL character in the command",
            ));
        }
        Ok(units)
    }

    /// Arguments quoted as the Microsoft C runtime parses them.
    pub(super) fn command_line(args: &[Vec<u16>]) -> Vec<u16> {
        const QUOTE: u16 = b'"' as u16;
        const BACKSLASH: u16 = b'\\' as u16;
        let mut line = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            if index > 0 {
                line.push(u16::from(b' '));
            }
            let plain = !arg.is_empty()
                && !arg
                    .iter()
                    .any(|unit| matches!(*unit, 0x20 | 0x09 | 0x0a | 0x0b | QUOTE));
            if plain {
                line.extend_from_slice(arg);
                continue;
            }
            line.push(QUOTE);
            let mut backslashes = 0_usize;
            for unit in arg {
                if *unit == BACKSLASH {
                    backslashes += 1;
                } else {
                    if *unit == QUOTE {
                        line.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
                    }
                    backslashes = 0;
                }
                line.push(*unit);
            }
            line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
            line.push(QUOTE);
        }
        line
    }

    /// The environment block: later values win, names compare
    /// case-insensitively, sorted as Windows expects.
    fn environment_block(env: &[(std::ffi::OsString, std::ffi::OsString)]) -> io::Result<Vec<u16>> {
        let upper = |units: &[u16]| -> Vec<u16> {
            units
                .iter()
                .map(|unit| {
                    if (0x61..=0x7a).contains(unit) {
                        unit - 0x20
                    } else {
                        *unit
                    }
                })
                .collect()
        };
        let mut variables: Vec<(Vec<u16>, Vec<u16>)> = Vec::new();
        for (name, value) in env {
            let name = wide_arg(name)?;
            let value = wide_arg(value)?;
            if name.is_empty() || name.contains(&u16::from(b'=')) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "an invalid environment variable name",
                ));
            }
            let key = upper(&name);
            variables.retain(|(existing, _)| upper(existing) != key);
            variables.push((name, value));
        }
        variables.sort_by_cached_key(|(name, _)| upper(name));
        let mut block = Vec::new();
        for (name, value) in &variables {
            block.extend_from_slice(name);
            block.push(u16::from(b'='));
            block.extend_from_slice(value);
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        Ok(block)
    }
}

/// The command-line quoting is pure and checked on every platform.
#[cfg(all(test, windows))]
mod tests {
    use super::windows::command_line;

    fn line(args: &[&str]) -> String {
        let units: Vec<Vec<u16>> = args
            .iter()
            .map(|arg| arg.encode_utf16().collect())
            .collect();
        String::from_utf16(&command_line(&units)).unwrap()
    }

    #[test]
    fn quotes_as_the_c_runtime_parses() {
        assert_eq!(line(&["a", "b c", ""]), r#"a "b c" """#);
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(line(&[r"dir\ with\", r"x\"]), r#""dir\ with\\" x\"#);
    }
}
